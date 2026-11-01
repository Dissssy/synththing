//! The local app API (docs/API.md, issue #145): what other programs on
//! this computer can ask of the running app, for integrations people build
//! themselves (home automation, notifications, game hooks). Off until it's
//! turned on in Preferences > Integrations, and then only reachable from
//! this computer, unless it's let in from the local network as well.
//!
//! Every app that uses it is given its own token (`Access`) with what it
//! may do: read (the library, what's playing, the event stream) and
//! control (run commands). Only a hash of each token is kept.
//!
//! The server runs on threads of its own. What only the app knows, it asks
//! the app for (`Call`), which answers between frames (`app/api.rs`); a
//! song search runs here, on a copy of the library the app hands over.
//! Events go out as Server-Sent Events to every open stream.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::net::SocketAddr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response};

use crate::playlist::LoopMode;
use crate::song_list::Autoplay;

use crate::commands::Command;
use crate::fields::{Facts, Field};
use crate::panel::{Given, Panel};
use crate::smart::{Group, Order, Rules, Ruleset, Split, SplitBy};
use crate::song_store::{self, Song, SongStore, Stored};
use crate::audio::SAMPLE_RATE;
use crate::audio_stream::Listeners;
use crate::visualizer_stream::Format;
use crate::lookup::matching::Release;
use crate::lookup::musicbrainz::{MusicBrainz, Query};
use crate::staged::Staged;
use crate::wishlist::{Kind, Wish};

/// The port it listens on unless it's given another.
pub const DEFAULT_PORT: u16 = 7340;
/// Request handlers (a stream gets a thread of its own besides).
const WORKERS: usize = 2;
/// Streams open at once.
const MOST_STREAMS: usize = 32;
/// How long a stream goes quiet before a keep-alive comment.
const KEEP_ALIVE: Duration = Duration::from_secs(25);
/// How long an audio stream waits for audio before it sends that long a
/// silence instead.
const QUIET: Duration = Duration::from_millis(250);
/// As `KEEP_ALIVE`, for a program with a panel: a stream only finds out
/// it's closed when it next writes, and the panel should grey soon after.
const PANEL_KEEP_ALIVE: Duration = Duration::from_secs(2);
/// The visualizer stream's frame rate, unless it asks for another.
const DEFAULT_FPS: u32 = 30;
/// How long `frame.jpg` waits for the visualizer to draw one.
const FRAME_WAIT: Duration = Duration::from_secs(3);
/// How long a request waits for the app to answer.
const APP_WAIT: Duration = Duration::from_secs(5);
/// The events there are (`GET /v1/events?events=` picks among them).
const EVENTS: [&str; 10] = [
    "now_playing",
    "seeked",
    "library_changed",
    "playlists_changed",
    "queue_changed",
    "soundfonts_changed",
    "scripts_changed",
    "wishlist_changed",
    "panel",
    "shared_changed",
];
/// Songs in one page of a list, unless the request asks for fewer.
const MOST_SONGS: usize = 500;
/// The largest song a program can send (as the relay's limit).
const LARGEST_SONG: usize = 250 << 20;

/// The API's preferences (`Config::api`), kept on this device only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub on: bool,
    /// `None`: `DEFAULT_PORT`.
    pub port: Option<u16>,
    pub apps: Vec<Access>,
    /// Listen on the local network too, not only this computer.
    pub lan: bool,
    /// Show synththing in the system's media controls
    /// (`media_controls.rs`): `None` means yes.
    pub media_controls: Option<bool>,
}

impl Settings {
    pub fn media_controls(&self) -> bool {
        self.media_controls.unwrap_or(true) && !cfg!(target_os = "android")
    }

    pub fn port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_PORT)
    }
}

/// An app allowed in: its name, its token's hash and what it may do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Access {
    pub name: String,
    /// SHA-256 of its token, in hex.
    pub token: String,
    pub read: bool,
    pub control: bool,
    /// May add songs to the library (`POST /v1/songs`). Never on unless
    /// the user ticks it.
    pub add_songs: bool,
    /// May change the library: star songs, tag them, add songs to
    /// playlists and take them out. Never on unless the user ticks it.
    pub edit_library: bool,
    /// May download songs' files (`GET /v1/songs/{key}/file`). Never on
    /// unless the user ticks it.
    pub download_songs: bool,
    /// May hear what plays (`GET /v1/audio/stream`). Never on unless the
    /// user ticks it.
    pub listen: bool,
    /// May show a panel in the Apps tab (`PUT /v1/panel`). Never on unless
    /// the user ticks it.
    pub panel: bool,
    /// Its panel shows on the user's other devices too (`panel_share.rs`),
    /// which can use it there. Never on unless the user ticks it.
    pub share: bool,
}

impl Access {
    /// A new app's access and the token to give it (shown once: only its
    /// hash is kept).
    pub fn new(name: &str) -> (Access, String) {
        let mut bytes = [0u8; 24];
        getrandom::fill(&mut bytes).expect("the OS's random numbers");
        let token = format!("st_{}", crate::library::hex(&bytes));
        let access = Access {
            name: name.to_string(),
            token: crate::library::sha256_hex(token.as_bytes()),
            read: true,
            control: false,
            add_songs: false,
            edit_library: false,
            download_songs: false,
            listen: false,
            panel: false,
            share: false,
        };
        (access, token)
    }
}

/// Something only the app can answer, with where to send the answer.
pub enum Call {
    /// Run a command: `Err` if it can't run now.
    Command(Command, Sender<Result<(), String>>),
    NowPlaying(Sender<NowPlaying>),
    /// A copy of the library and its playlists, to search.
    Library(Sender<Library>),
    /// Play a song (by its key), or a playlist (by its ID) from the top or
    /// from one of its songs: `Err` (with its status) if there's no such
    /// song or playlist.
    Play {
        song: Option<String>,
        playlist: Option<String>,
        reply: Sender<Result<(), (u16, String)>>,
    },
    /// Change the library: `Err` (with its status) if it can't be.
    Edit(Edit, Sender<Result<(), (u16, String)>>),
    /// Set where the song is or how loud it plays: `Err` (with its status)
    /// if it can't be now.
    Set(Set, Sender<Result<(), (u16, String)>>),
    /// Change the play queue: `Err` (with its status) if it can't be.
    Queue(QueueEdit, Sender<Result<(), (u16, String)>>),
    /// Make a playlist with this name: it, or `Err` (with its status).
    NewPlaylist(String, Sender<Result<PlaylistRef, (u16, String)>>),
    /// Use the soundfont with this ID: `Err` (with its status) if it can't
    /// be.
    UseSoundfont(String, Sender<Result<(), (u16, String)>>),
    /// Draw the visualizer with the script with this ID: `Err` (with its
    /// status) if it can't be.
    UseScript(String, Sender<Result<(), (u16, String)>>),
    /// A song a program sent (`program`, by its name), already in the
    /// store's folder, to add to the library: the song, and whether it's
    /// new to the library (only then is it marked as the program's), with
    /// its match at MusicBrainz if it was matched while staged (kept unless
    /// the library's song is matched already).
    Add {
        stored: Stored,
        program: String,
        online: Option<song_store::Online>,
        reply: Sender<Result<(SongInfo, bool), String>>,
    },
    /// The wishlist, those `query` keeps (`wishlist_search`): its wishes
    /// with what of each is here, an artist's albums under them.
    Wishlist(String, Sender<Vec<WishInfo>>),
    /// Wish for `wish`, for `program` (`release` its album's edition, as
    /// MusicBrainz said, to keep): it, and whether it's new to the
    /// wishlist; `Err` (with its status) if it can't be.
    Wish {
        wish: Wish,
        release: Option<Release>,
        program: String,
        reply: Sender<Result<(WishInfo, bool), (u16, String)>>,
    },
}

/// A wish, as `/v1/wishlist` has it: what was wished for (`Wish`'s
/// fields), its ID, and what of it is here: how much (`got`: no, partly
/// or yes; `have` of `total` tracks, or an artist's albums), its songs
/// (by key) and an artist's albums wished for.
#[derive(Clone, Debug, Serialize)]
pub struct WishInfo {
    pub id: String,
    #[serde(flatten)]
    pub wish: Wish,
    pub got: &'static str,
    pub have: usize,
    pub total: usize,
    pub songs: Vec<String>,
    /// An album's tracks still missing, by title, when its track list is
    /// known.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub albums: Vec<WishInfo>,
}

/// A release at MusicBrainz as the API gives it: its tracks, each with
/// the recording that fills a song wish (and an album wish's `skipped`).
#[derive(Clone, Debug, Serialize)]
pub struct ReleaseInfo {
    pub id: String,
    pub group: Option<String>,
    pub title: String,
    pub artist: String,
    pub date: Option<String>,
    pub tracks: Vec<TrackInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TrackInfo {
    pub disc: u32,
    pub position: u32,
    pub title: String,
    pub length: Option<f64>,
    pub recording: Option<String>,
}

impl From<Release> for ReleaseInfo {
    fn from(release: Release) -> Self {
        let tracks = release.tracks.into_iter().map(|t| TrackInfo { disc: t.disc, position: t.position, title: t.title, length: t.length, recording: t.recording }).collect();
        ReleaseInfo { id: release.id, group: release.group, title: release.title, artist: release.artist, date: release.date, tracks }
    }
}

/// The wishes of `list` that `query` keeps (as the Wishlist's filter
/// does), newest first, each artist's albums under them, with what of
/// each is here (`progress`).
pub fn wishes(list: &crate::wishlist::Wishlist, progress: &std::collections::BTreeMap<String, crate::wishlist::Progress>, query: &str) -> Vec<WishInfo> {
    let wishes = &list.wishes;
    let filter = crate::wishlist_search::Filter::parse(query);
    let now = crate::settings_sync::now();
    let state = |id: &str| progress.get(id).map_or(crate::wishlist::State::Wanted, |p| p.state());
    let kept = |id: &String| filter.keeps(&wishes[id], state(id), now);
    let info = |id: &String, albums| info_of(id, &wishes[id], progress.get(id), albums);
    let mut top: Vec<&String> = wishes
        .iter()
        .filter(|(_, w)| w.under.as_ref().is_none_or(|artist| !wishes.contains_key(artist)))
        .map(|(id, _)| id)
        .filter(|id| kept(id) || list.under(id).any(|(album, _)| kept(album)))
        .collect();
    top.sort_by_key(|id| std::cmp::Reverse(wishes[*id].added));
    top.into_iter()
        .map(|id| {
            let mut albums: Vec<(&String, &Wish)> = list.under(id).filter(|(album, _)| kept(album)).collect();
            albums.sort_by(|(_, a), (_, b)| (&a.year, &a.title).cmp(&(&b.year, &b.title)));
            info(id, albums.into_iter().map(|(album, _)| info(album, Vec::new())).collect())
        })
        .collect()
}

/// Wish for `wish` on `list` for `program`: its ID, and whether it's new
/// to the wishlist. `under` must be an artist's wish, and only an album
/// goes under one.
pub fn wish(list: &mut crate::wishlist::Wishlist, mut wish: Wish, program: &str) -> Result<(String, bool), (u16, String)> {
    if let Some(artist) = &wish.under {
        match list.wishes.get(artist) {
            None => return Err((404, "there's no wish with that \"under\" ID".into())),
            Some(w) if w.kind != Kind::Artist => return Err((400, "\"under\" is an artist's wish's ID".into())),
            Some(_) if wish.kind != Kind::Album => return Err((400, "only an album goes under an artist".into())),
            Some(_) => {}
        }
    }
    let new = list.find(&wish).is_none();
    wish.added_by = Some(program.to_string());
    Ok((list.add(wish), new))
}

/// The wish `id` of `list` as the API has it, an artist's with its
/// albums.
pub fn wish_info(list: &crate::wishlist::Wishlist, progress: &std::collections::BTreeMap<String, crate::wishlist::Progress>, id: &str) -> WishInfo {
    let albums = list.under(id).map(|(album, w)| info_of(album, w, progress.get(album), Vec::new())).collect();
    info_of(id, &list.wishes[id], progress.get(id), albums)
}

fn info_of(id: &str, wish: &Wish, progress: Option<&crate::wishlist::Progress>, albums: Vec<WishInfo>) -> WishInfo {
    use crate::wishlist::State;
    let progress = progress.cloned().unwrap_or_default();
    let got = match progress.state() {
        _ if progress.total == 0 => "no",
        State::Wanted => "no",
        State::Partly => "partly",
        State::Got => "yes",
    };
    WishInfo { id: id.to_string(), wish: wish.clone(), got, have: progress.have, total: progress.total, songs: progress.songs, missing: progress.missing, albums }
}

/// The playlists of `playlists` as the API has them, their songs by key
/// (those of songs not in `songs`' folder left out).
pub fn playlist_infos(playlists: &crate::playlist::Library, songs: &SongStore) -> Vec<PlaylistInfo> {
    let dir = songs.dir();
    playlists
        .lists
        .iter()
        .map(|list| PlaylistInfo {
            id: list.id.clone(),
            name: list.playlist.name.clone(),
            smart: list.playlist.smart.is_some(),
            songs: list.playlist.entries.iter().filter_map(|e| song_store::key_in(dir, &e.path).map(str::to_string)).collect(),
        })
        .collect()
}

/// A change to the play queue a program asks for (`Call::Queue`).
#[derive(Clone, Debug, PartialEq)]
pub enum QueueEdit {
    /// Queue a song (by its key) or a playlist's songs (by its ID): next,
    /// or at the end.
    Add { song: Option<String>, playlist: Option<String>, next: bool },
    /// Take out the song at this place in the queue.
    Remove(usize),
    /// Move the song at `from` so it ends up at `to`.
    Move { from: usize, to: usize },
    Clear,
}

/// A change to the library a program asks for (`Call::Edit`).
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Star song `key` (`on`), or unstar it.
    Star { key: String, on: bool },
    /// Give song `key` the tag named `tag` (`on`), or take it off.
    Tag { key: String, tag: String, on: bool },
    /// Add song `key` to the end of playlist `playlist` (`on`), or take
    /// every entry of it out.
    Playlist { playlist: String, key: String, on: bool },
    /// Rename playlist `playlist`.
    Rename { playlist: String, name: String },
    /// Delete playlist `playlist`.
    Delete { playlist: String },
    /// Move the entry at `from` of playlist `playlist` so it ends up at
    /// `to`.
    Move { playlist: String, from: usize, to: usize },
    /// Take wish `id` off the wishlist (an artist's albums with them).
    Unwish { id: String },
    /// Write `note` on wish `id`.
    Note { id: String, note: String },
}

/// What `Call::Set` sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Set {
    /// Seconds into the song.
    Position(f64),
    /// 0 to 1.
    Volume(f32),
    /// Shuffle, Loop and Autoplay: the ones given.
    Modes { shuffle: Option<bool>, loop_mode: Option<LoopMode>, autoplay: Option<Autoplay> },
}

/// Loop as the API names it.
pub fn loop_name(mode: LoopMode) -> &'static str {
    match mode {
        LoopMode::Off => "off",
        LoopMode::One => "one",
        LoopMode::All => "all",
    }
}

/// Autoplay as the API names it.
pub fn autoplay_name(mode: Autoplay) -> &'static str {
    match mode {
        Autoplay::Off => "off",
        Autoplay::Group => "group",
        Autoplay::All => "all",
    }
}

/// What's playing, as `GET /v1/now-playing` and the `now_playing` event
/// give it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct NowPlaying {
    pub song: Option<SongInfo>,
    /// The playlist it's playing from, if any.
    pub playlist: Option<PlaylistRef>,
    pub paused: bool,
    /// Seconds.
    pub position: f64,
    pub length: f64,
    pub speed: f64,
    /// 0 to 1.
    pub volume: f32,
    pub shuffle: bool,
    /// `off`, `one` (this song) or `all` (the playlist).
    #[serde(rename = "loop")]
    pub loop_mode: &'static str,
    /// What a song played from a list plays on through: `off`, `group` or
    /// `all`.
    pub autoplay: &'static str,
    /// The soundfont the MIDI song playing plays with (`None` for any
    /// other song).
    pub soundfont: Option<PlaylistRef>,
}

/// A soundfont in the Soundfonts list, as `GET /v1/soundfonts` gives it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SoundfontInfo {
    pub id: String,
    pub name: String,
    /// Bytes, or `None` if its file is missing.
    pub size: Option<u64>,
    /// Whether it's the one MIDI songs play with.
    pub active: bool,
}

/// A visualizer script in the script picker, as `GET /v1/scripts` gives it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScriptInfo {
    pub id: String,
    pub name: String,
    /// Whether it's the one drawing the visualizer.
    pub active: bool,
}

/// A soundfont's or script's ID: the same for its file every time.
pub fn file_id(path: &std::path::Path) -> String {
    use sha2::Digest;
    crate::library::hex(&sha2::Sha256::digest(path.to_string_lossy().as_bytes())[..8])
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlaylistRef {
    pub id: String,
    pub name: String,
}

/// The library as the app has it, to search away from the app's thread.
pub struct Library {
    pub store: SongStore,
    /// The wishlist, for `is:wished` and `wish:`.
    pub wishes: crate::wishlist::Wishes,
    pub playlists: Vec<PlaylistInfo>,
    /// Every ruleset, by ID: the built-in ones, then the user's.
    pub rulesets: Vec<(String, Ruleset)>,
    /// How groups are told apart and sorted (`Config::group_ignore`,
    /// `Config::sort_with_articles`).
    pub group_ignore: String,
    pub articles: bool,
    /// Whether covers not kept yet may be downloaded.
    pub online: bool,
    /// The play queue's songs, by key, in order.
    pub queue: Vec<String>,
    pub soundfonts: Vec<SoundfontInfo>,
    pub scripts: Vec<ScriptInfo>,
}

impl Library {
    /// The playlists `rules` make of the songs on this device (as
    /// `/v1/songs` has them: a song known only from another device is
    /// left out, and a playlist of none of these with it).
    fn groups(&self, rules: &Rules) -> Vec<GroupInfo> {
        let groups = rules.groups(&self.store, &self.wishes, crate::settings_sync::now(), &self.group_ignore, self.articles);
        groups
            .into_iter()
            .filter_map(|mut group| {
                group.songs.retain(|key| self.store.has(key));
                (!group.songs.is_empty()).then(|| group.into())
            })
            .collect()
    }
}

/// A ruleset as `GET /v1/groupings` lists it.
#[derive(Serialize)]
struct GroupingInfo<'a> {
    id: &'a str,
    name: &'a str,
    /// What it splits its songs by.
    split: Option<SplitBy>,
}

/// One of the playlists a ruleset makes.
#[derive(Serialize)]
struct GroupInfo {
    /// What tells it apart from the ruleset's others, kept while its name
    /// is (a hash of `Group::key`).
    id: String,
    title: String,
    /// An album's artist.
    subtitle: Option<String>,
    /// By key, in order.
    songs: Vec<String>,
}

impl From<Group> for GroupInfo {
    fn from(group: Group) -> Self {
        let id = crate::library::sha256_hex(group.key.as_bytes())[..16].to_string();
        GroupInfo { id, title: group.title, subtitle: group.subtitle, songs: group.songs }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlaylistInfo {
    pub id: String,
    pub name: String,
    pub smart: bool,
    /// Its songs, by key, in order.
    pub songs: Vec<String>,
}

/// A song as the API gives it: by key, with its fields as the user sees
/// them (edits over the file's tags).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SongInfo {
    pub key: String,
    pub file_name: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i64>,
    pub track: Option<i64>,
    /// Seconds.
    pub length: Option<f64>,
    pub starred: bool,
    /// Its tags' names.
    pub tags: Vec<String>,
    /// When it came into the library (Unix seconds).
    pub imported: i64,
    /// The program that added it through this API, if one did.
    pub added_by: Option<String>,
    /// What it is at MusicBrainz (its match, else what its file says):
    /// its recording, release and the release's group.
    pub recording: Option<String>,
    pub release: Option<String>,
    pub release_group: Option<String>,
}

impl SongInfo {
    pub fn of(key: &str, song: &Song, store: &SongStore) -> SongInfo {
        let facts = Facts::of(song);
        let text = |field| facts.text(field).map(|t| t.into_owned());
        SongInfo {
            key: key.to_string(),
            file_name: song.name.clone(),
            title: text(Field::Title),
            artist: text(Field::Artist),
            album: text(Field::Album),
            album_artist: text(Field::AlbumArtist),
            genre: text(Field::Genre),
            year: facts.number(Field::Year).map(|n| n as i64),
            track: facts.number(Field::Track).map(|n| n as i64),
            length: facts.number(Field::Length),
            starred: facts.starred(),
            tags: song.tags.iter().filter_map(|id| store.tags().get(id)).map(|t| t.name.clone()).collect(),
            imported: song.imported,
            added_by: song.added_by.clone(),
            recording: facts.recording().map(str::to_string),
            release: facts.release().map(str::to_string),
            release_group: song.online.as_ref().and_then(|o| o.group.clone()),
        }
    }
}

/// The running server. Stopped when dropped.
pub struct Server {
    addr: SocketAddr,
    http: Arc<tiny_http::Server>,
    shared: Arc<Shared>,
    calls: Receiver<Call>,
    threads: Vec<JoinHandle<()>>,
}

struct Shared {
    /// Let in from the local network, not only this computer.
    lan: bool,
    apps: RwLock<Vec<Access>>,
    calls: Mutex<Sender<Call>>,
    /// Woken when a call comes in, so it's answered even while nothing's
    /// being drawn.
    waker: egui::Context,
    streams: Mutex<Vec<Stream>>,
    panels: Mutex<Panels>,
    /// The visualizer's pictures, for whoever's watching them.
    frames: Arc<crate::visualizer_stream::Frames>,
    /// What plays, for whoever's listening: `None` where nothing plays (a
    /// hub).
    listeners: Option<Arc<Listeners>>,
    /// What changed in the library when, for `/v1/songs?since=`.
    changes: Mutex<Changes>,
    /// Songs programs sent to be checked before they're imported.
    staged: Mutex<Staged>,
}

/// An open event stream.
struct Stream {
    /// Tells it from the others, to take it out when it closes.
    id: u64,
    /// The program's token's hash (`Access::token`): its panel's events
    /// go only to its own streams.
    app: String,
    /// The events it wants (`None`: all).
    wanted: Option<Vec<String>>,
    tx: Sender<String>,
}

impl Stream {
    fn wants(&self, name: &str) -> bool {
        self.wanted.as_ref().is_none_or(|w| w.iter().any(|e| e == name))
    }
}

/// The programs' panels, in the order they first came, and a count that
/// goes up with every change, so the app only looks again after one. It
/// starts at the time (in ms), so a value given after a restart counts as
/// newer on other devices too.
struct Panels {
    revision: u64,
    shown: Vec<ShownPanel>,
}

impl Default for Panels {
    fn default() -> Self {
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        Panels { revision: ms, shown: Vec::new() }
    }
}

/// A program's panel, as the Apps tab shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ShownPanel {
    /// The program's token's hash (`Access::token`).
    pub app: String,
    /// The program's name, as the user typed it.
    pub name: String,
    pub panel: Panel,
    /// The latest value the program gave each textbox, and when: each is
    /// taken once (by whoever shows the panel), so a later change elsewhere
    /// in the panel doesn't put it back over what's been typed since.
    pub given: Vec<Given>,
    /// The panel as sent, for `PATCH` to change (and other devices to
    /// show).
    pub source: serde_json::Value,
    /// It shows on the user's other devices too (`Access::share`).
    pub share: bool,
}

impl ShownPanel {
    /// Note the values given to textboxes, as of `revision`.
    fn give(&mut self, values: Vec<(String, String)>, revision: u64) {
        for (id, value) in values {
            self.given.retain(|g| g.id != id);
            self.given.push(Given { id, value, revision });
        }
    }
}

/// The library's revisions while synththing runs: each song as it was
/// last seen (a hash of it as the API gives it) and the revision it last
/// changed in, and each song gone and when. A revision is `<run>.<n>`,
/// `run` telling this run of synththing from the others (a revision from
/// another one means nothing here).
struct Changes {
    run: u32,
    revision: u64,
    seen: HashMap<String, (u64, u64)>,
    gone: HashMap<String, u64>,
}

impl Changes {
    fn new() -> Self {
        let run = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.subsec_nanos() ^ t.as_secs() as u32);
        Changes { run, revision: 0, seen: HashMap::new(), gone: HashMap::new() }
    }

    /// Catch up with `store`: a new revision if any song came, changed or
    /// went since last time.
    fn update(&mut self, store: &SongStore) {
        use std::hash::{Hash, Hasher};
        let next = self.revision + 1;
        let mut changed = false;
        let mut here = std::collections::HashSet::new();
        for (path, song) in store.songs() {
            let Some(key) = song_store::key_in(store.dir(), &path) else { continue };
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_string(&SongInfo::of(key, song, store)).unwrap_or_default().hash(&mut hasher);
            let hash = hasher.finish();
            let key = key.to_string();
            if self.seen.get(&key).is_none_or(|(seen, _)| *seen != hash) {
                self.seen.insert(key.clone(), (hash, next));
                self.gone.remove(&key);
                changed = true;
            }
            here.insert(key);
        }
        let went: Vec<String> = self.seen.keys().filter(|key| !here.contains(*key)).cloned().collect();
        for key in went {
            self.seen.remove(&key);
            self.gone.insert(key, next);
            changed = true;
        }
        if changed {
            self.revision = next;
        }
    }

    fn revision(&self) -> String {
        format!("{}.{}", self.run, self.revision)
    }

    /// `since` as a revision of this run, if it is one.
    fn since(&self, since: &str) -> Option<u64> {
        let (run, n) = since.split_once('.')?;
        (run.parse() == Ok(self.run)).then(|| n.parse().ok()).flatten().filter(|n| *n <= self.revision)
    }

    fn changed_after(&self, key: &str, since: u64) -> bool {
        self.seen.get(key).is_some_and(|(_, rev)| *rev > since)
    }
}

impl Server {
    /// Listen on `port`, on this computer only unless `lan`, staging songs
    /// in `staging` (emptied first).
    pub fn start(port: u16, lan: bool, apps: Vec<Access>, waker: egui::Context, staging: std::path::PathBuf, listeners: Option<Arc<Listeners>>) -> Result<Server, String> {
        let http = tiny_http::Server::http((if lan { "0.0.0.0" } else { "127.0.0.1" }, port)).map_err(|e| format!("couldn't listen on port {port}: {e}"))?;
        let addr = http.server_addr().to_ip().ok_or("not listening on an IP address")?;
        let http = Arc::new(http);
        let (tx, calls) = mpsc::channel();
        let shared = Arc::new(Shared { lan, apps: RwLock::new(apps), calls: Mutex::new(tx), waker, streams: Mutex::default(), panels: Mutex::default(), frames: crate::visualizer_stream::Frames::new(), listeners, changes: Mutex::new(Changes::new()), staged: Mutex::new(Staged::new(staging)) });
        let threads = (0..WORKERS)
            .map(|_| {
                let (http, shared) = (Arc::clone(&http), Arc::clone(&shared));
                std::thread::Builder::new()
                    .name("api".into())
                    .spawn(move || {
                        while let Ok(request) = http.recv() {
                            handle(&shared, request);
                        }
                    })
                    .map_err(|e| format!("couldn't start: {e}"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Server { addr, http, shared, calls, threads })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The visualizer's pictures, for the API's watchers.
    pub fn frames(&self) -> &Arc<crate::visualizer_stream::Frames> {
        &self.shared.frames
    }

    /// The apps allowed in, as the preferences now have them. A panel
    /// whose program is gone, or may no longer show one, goes with it.
    pub fn set_apps(&self, apps: &[Access]) {
        if *self.shared.apps.read().unwrap_or_else(|p| p.into_inner()) != apps {
            *self.shared.apps.write().unwrap_or_else(|p| p.into_inner()) = apps.to_vec();
            let mut panels = lock(&self.shared.panels);
            let before = panels.shown.clone();
            panels.shown.retain(|shown| apps.iter().any(|a| a.token == shown.app && a.panel));
            for shown in &mut panels.shown {
                shown.share = apps.iter().any(|a| a.token == shown.app && a.share);
            }
            if panels.shown != before {
                panels.revision += 1;
            }
        }
    }

    /// The programs' panels, if they changed since `revision` (which then
    /// moves on to now).
    pub fn panels(&self, revision: &mut u64) -> Option<Vec<ShownPanel>> {
        let panels = lock(&self.shared.panels);
        (panels.revision != *revision).then(|| {
            *revision = panels.revision;
            panels.shown.clone()
        })
    }

    /// Take a program's panel away (the user closed it).
    pub fn close_panel(&self, app: &str) {
        let mut panels = lock(&self.shared.panels);
        panels.shown.retain(|shown| shown.app != app);
        panels.revision += 1;
    }

    /// Whether a program (by its token's hash) has an event stream open:
    /// only then does it hear its panel.
    pub fn connected(&self, app: &str) -> bool {
        lock(&self.shared.streams).iter().any(|stream| stream.app == app)
    }

    /// Tell a program (by its token's hash) what the user did with its
    /// panel.
    pub fn panel_event(&self, app: &str, event: &crate::panel::Event) {
        self.send("panel", event, |stream| stream.app == app);
    }

    /// The calls waiting for the app.
    pub fn calls(&self) -> impl Iterator<Item = Call> + '_ {
        self.calls.try_iter()
    }

    /// Whether any event stream is open (events are only worked out then).
    pub fn listened_to(&self) -> bool {
        !lock(&self.shared.streams).is_empty()
    }

    /// Send an event to every open stream.
    pub fn event(&self, name: &str, data: &impl Serialize) {
        self.send(name, data, |_| true);
    }

    /// Send an event to the open streams `to` picks that want it (and let
    /// go of those that closed).
    fn send(&self, name: &str, data: &impl Serialize, to: impl Fn(&Stream) -> bool) {
        let Ok(data) = serde_json::to_string(data) else { return };
        let message = format!("event: {name}\ndata: {data}\n\n");
        lock(&self.shared.streams).retain(|stream| !(to(stream) && stream.wants(name)) || stream.tx.send(message.clone()).is_ok());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // (Each unblock frees one worker; dropping the senders ends the
        // streams.)
        for _ in &self.threads {
            self.http.unblock();
        }
        lock(&self.shared.streams).clear();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

type Reply = Response<Cursor<Vec<u8>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

#[derive(Serialize)]
struct ApiError {
    error: String,
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("plain ASCII header")
}

fn json<T: Serialize>(status: u16, body: &T) -> Reply {
    Response::from_data(serde_json::to_vec(body).unwrap_or_default())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json; charset=utf-8"))
}

fn error(status: u16, message: impl Into<String>) -> Reply {
    json(status, &ApiError { error: message.into() })
}

/// What a request may do: the access of the app whose token it carries
/// (`Authorization: Bearer <token>`, or `?token=` where a header can't be
/// set, as from a browser's `EventSource`).
fn access(shared: &Shared, request: &Request, query: &HashMap<String, String>) -> Option<Access> {
    let bearer = request.headers().iter().find(|h| h.field.equiv("Authorization")).and_then(|h| h.value.as_str().strip_prefix("Bearer ")).map(str::trim);
    let token = bearer.or(query.get("token").map(String::as_str))?;
    let hash = crate::library::sha256_hex(token.as_bytes());
    shared.apps.read().unwrap_or_else(|p| p.into_inner()).iter().find(|a| a.token == hash).cloned()
}

/// Whether a request came from a program on this computer itself, not
/// passed on by a proxy here (which could be letting in anyone) or sent by
/// a web page that renamed itself to this computer's address: no proxy's
/// headers, and addressed to this computer by name.
fn from_here(request: &Request) -> bool {
    const PROXIED: [&str; 4] = ["Forwarded", "X-Forwarded-For", "X-Forwarded-Host", "X-Real-IP"];
    let headers = request.headers();
    if headers.iter().any(|h| PROXIED.iter().any(|p| h.field.equiv(*p))) {
        return false;
    }
    headers.iter().find(|h| h.field.equiv("Host")).is_some_and(|h| {
        let host = h.value.as_str();
        // (The port, if any, after the last colon outside brackets.)
        let name = match host.rsplit_once(':') {
            Some((name, port)) if !port.contains(']') => name,
            _ => host,
        };
        ["localhost", "127.0.0.1", "[::1]"].iter().any(|n| name.eq_ignore_ascii_case(n))
    })
}

/// The app's answer to something it was asked to do: 204 when done.
fn answered(answer: Result<Result<(), (u16, String)>, Reply>) -> Reply {
    match answer {
        Ok(Ok(())) => Response::from_data(Vec::new()).with_status_code(204),
        Ok(Err((status, why))) => error(status, why),
        Err(reply) => reply,
    }
}

/// `?events=now_playing,seeked`: the events a stream wants (`None`: all).
fn wanted_events(query: &HashMap<String, String>) -> Result<Option<Vec<String>>, Reply> {
    let Some(named) = query.get("events") else { return Ok(None) };
    let wanted: Vec<String> = named.split(',').map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).collect();
    match wanted.iter().find(|e| !EVENTS.contains(&e.as_str())) {
        Some(unknown) => Err(error(400, format!("there's no event \"{unknown}\": {}", EVENTS.join(", ")))),
        None => Ok(Some(wanted)),
    }
}

/// Ask the app, and wait for its answer.
fn ask<T>(shared: &Shared, call: impl FnOnce(Sender<T>) -> Call) -> Result<T, Reply> {
    let (tx, rx) = mpsc::channel();
    if shared.calls.lock().unwrap_or_else(|p| p.into_inner()).send(call(tx)).is_err() {
        return Err(error(503, "synththing is closing"));
    }
    shared.waker.request_repaint();
    rx.recv_timeout(APP_WAIT).map_err(|_| error(503, "synththing didn't answer in time"))
}

fn handle(shared: &Arc<Shared>, mut request: Request) {
    let url = request.url().to_string();
    let (path, raw_query) = url.split_once('?').unwrap_or((&url, ""));
    let query = crate::library::server::parse_query(raw_query);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let method = request.method().clone();
    if !shared.lan && !from_here(&request) {
        let _ = request.respond(error(403, "only programs on this computer may use the API (Preferences > Integrations can let the local network in)"));
        return;
    }
    let Some(access) = access(shared, &request, &query) else {
        let _ = request.respond(error(401, "a token is needed: make one in Preferences > Integrations"));
        return;
    };
    let needs = |allowed: bool, what: &str| (!allowed).then(|| error(403, format!("this app isn't allowed to {what}")));
    let reply = match (&method, parts.as_slice()) {
        (Method::Get, ["v1"]) => Some(json(
            200,
            &serde_json::json!({
                "app": "synththing",
                "version": crate::build_info::VERSION,
                "read": access.read,
                "control": access.control,
                "add_songs": access.add_songs,
                "edit_library": access.edit_library,
                "download_songs": access.download_songs,
                "listen": access.listen,
                "panel": access.panel,
                "share": access.share,
            }),
        )),
        (Method::Get, ["v1", "events"]) => match needs(access.read, "read").map_or_else(|| wanted_events(&query), Err) {
            Err(reply) => Some(reply),
            Ok(wanted) => {
                stream(shared, request, access.token.clone(), wanted);
                return;
            }
        },
        (Method::Delete, ["v1", "events"]) => {
            // (A program closing down says so, and its panel greys at once.)
            lock(&shared.streams).retain(|stream| stream.app != access.token);
            shared.waker.request_repaint();
            Some(Response::from_data(Vec::new()).with_status_code(204))
        }
        (Method::Get, ["v1", "visualizer"]) => {
            // (With ?audio=1, it plays what synththing does too.)
            let audio = query.get("audio").is_some_and(|a| a == "1");
            Some(needs(access.read, "read").or_else(|| needs(access.listen || !audio, "listen")).unwrap_or_else(|| page("visualizer", raw_query, true, audio)))
        }
        (Method::Get, ["v1", "audio"]) => Some(needs(access.listen, "listen").unwrap_or_else(|| page("audio", raw_query, false, true))),
        (Method::Get, ["v1", "audio", "stream"]) => match needs(access.listen, "listen") {
            Some(reply) => Some(reply),
            None => match shared.listeners.as_ref().map(|l| l.listen()) {
                None => Some(error(404, "nothing plays here")),
                Some(None) => Some(error(503, format!("{} programs are listening already", crate::audio_stream::MOST_LISTENERS))),
                Some(Some(chunks)) => {
                    listen(request, chunks);
                    return;
                }
            },
        },
        (Method::Get, ["v1", "visualizer", "stream"]) => match needs(access.read, "read").map_or_else(|| watch_options(&query), Err) {
            Err(reply) => Some(reply),
            Ok((size, fps, format)) => {
                video(shared, request, size, fps, format);
                return;
            }
        },
        (Method::Get, ["v1", "visualizer", name @ ("frame.jpg" | "frame.png")]) => {
            let format = if *name == "frame.png" { Format::Png } else { Format::Jpeg };
            Some(needs(access.read, "read").unwrap_or_else(|| match watch_options(&query) {
                Err(reply) => reply,
                Ok((size, ..)) => {
                    let watching = shared.frames.watch(size, format);
                    let seen = shared.frames.count();
                    shared.waker.request_repaint();
                    let frame = shared.frames.next(seen, FRAME_WAIT, format);
                    drop(watching);
                    match frame {
                        Some((picture, _)) => Response::from_data(picture.to_vec())
                            .with_header(Header::from_bytes("Content-Type", format.content_type()).unwrap())
                            .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap()),
                        None => error(503, "the visualizer didn't draw a frame in time"),
                    }
                }
            }))
        }
        (Method::Post, ["v1", "seek"]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| {
            match query.get("to").and_then(|t| t.parse::<f64>().ok()).filter(|t| t.is_finite() && *t >= 0.0) {
                None => error(400, "say where to: ?to=<seconds>"),
                Some(to) => answered(ask(shared, |reply| Call::Set(Set::Position(to), reply))),
            }
        })),
        (Method::Post, ["v1", "volume"]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| {
            match query.get("to").and_then(|t| t.parse::<f32>().ok()).filter(|t| (0.0..=1.0).contains(t)) {
                None => error(400, "say how loud, from 0 to 1: ?to=0.5"),
                Some(to) => answered(ask(shared, |reply| Call::Set(Set::Volume(to), reply))),
            }
        })),
        (Method::Post, ["v1", "modes"]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| {
            let shuffle = query.get("shuffle").map(|v| v.parse::<bool>().map_err(|_| "shuffle is true or false"));
            let loop_mode = query.get("loop").map(|v| {
                [LoopMode::Off, LoopMode::One, LoopMode::All].into_iter().find(|m| loop_name(*m) == v).ok_or("loop is off, one or all")
            });
            let autoplay = query.get("autoplay").map(|v| {
                [Autoplay::Off, Autoplay::Group, Autoplay::All].into_iter().find(|m| autoplay_name(*m) == v).ok_or("autoplay is off, group or all")
            });
            match (shuffle.transpose(), loop_mode.transpose(), autoplay.transpose()) {
                (Err(why), _, _) | (_, Err(why), _) | (_, _, Err(why)) => error(400, why),
                (Ok(None), Ok(None), Ok(None)) => error(400, "say what to set: ?shuffle=, ?loop= or ?autoplay="),
                (Ok(shuffle), Ok(loop_mode), Ok(autoplay)) => answered(ask(shared, |reply| Call::Set(Set::Modes { shuffle, loop_mode, autoplay }, reply))),
            }
        })),
        (Method::Put, ["v1", "panel"]) => Some(needs(access.panel, "show a panel").unwrap_or_else(|| set_panel(shared, &mut request, &access))),
        (Method::Patch, ["v1", "panel", "elements", id]) => {
            Some(needs(access.panel, "show a panel").unwrap_or_else(|| patch_panel(shared, &mut request, &access, id)))
        }
        (Method::Delete, ["v1", "panel"]) => Some(needs(access.panel, "show a panel").unwrap_or_else(|| {
            let mut panels = lock(&shared.panels);
            panels.shown.retain(|shown| shown.app != access.token);
            panels.revision += 1;
            shared.waker.request_repaint();
            Response::from_data(Vec::new()).with_status_code(204)
        })),
        (Method::Get, ["v1", "commands"]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            let commands: Vec<_> = Command::ALL.iter().map(|c| serde_json::json!({ "id": c.id(), "name": c.name() })).collect();
            json(200, &commands)
        })),
        (Method::Post, ["v1", "commands", id]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| match Command::by_id(id) {
            None => error(404, format!("there's no command \"{id}\"")),
            Some(command) => match ask(shared, |tx| Call::Command(command, tx)) {
                Ok(Ok(())) => Response::from_data(Vec::new()).with_status_code(204),
                Ok(Err(why)) => error(409, why),
                Err(reply) => reply,
            },
        })),
        (Method::Post, ["v1", "play"]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| {
            let (song, playlist) = (query.get("song").cloned(), query.get("playlist").cloned());
            if song.is_none() && playlist.is_none() {
                return error(400, "say what to play: ?song=<key> or ?playlist=<id>");
            }
            answered(ask(shared, |reply| Call::Play { song, playlist, reply }))
        })),
        (Method::Get, ["v1", "queue"]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            ask(shared, Call::Library).map_or_else(
                |reply| reply,
                |library| {
                    let songs: Vec<SongInfo> = library.queue.iter().filter_map(|key| Some(SongInfo::of(key, library.store.get_by_key(key)?, &library.store))).collect();
                    json(200, &songs)
                },
            )
        })),
        (Method::Post, ["v1", "queue"]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| {
            let (song, playlist) = (query.get("song").cloned(), query.get("playlist").cloned());
            if song.is_none() == playlist.is_none() {
                return error(400, "say what to queue: ?song=<key> or ?playlist=<id>");
            }
            let next = match query.get("next").map(String::as_str) {
                None | Some("false") => false,
                Some("true") => true,
                Some(_) => return error(400, "?next= is true or false"),
            };
            answered(ask(shared, |reply| Call::Queue(QueueEdit::Add { song, playlist, next }, reply)))
        })),
        (Method::Get, ["v1", "shared"]) => Some(needs(access.read, "read").unwrap_or_else(|| json(200, &crate::shared_data::all()))),
        (Method::Get, ["v1", "shared", key]) => Some(needs(access.read, "read").unwrap_or_else(|| match crate::shared_data::get(&path_part(key)) {
            Some(value) => json(200, &value),
            None => error(404, "nothing's shared under that key"),
        })),
        (Method::Put, ["v1", "shared", key]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| share(shared, &mut request, &path_part(key)))),
        (Method::Delete, ["v1", "shared", key]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| {
            match crate::shared_data::set(&path_part(key), None) {
                Ok(true) => {
                    shared.waker.request_repaint();
                    Response::from_data(Vec::new()).with_status_code(204)
                }
                Ok(false) => error(404, "nothing's shared under that key"),
                Err(e) => error(400, e),
            }
        })),
        (Method::Delete, ["v1", "queue"]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| {
            let edit = match query.get("at").map(|at| at.parse::<usize>()) {
                None => QueueEdit::Clear,
                Some(Ok(at)) => QueueEdit::Remove(at),
                Some(Err(_)) => return error(400, "?at= is a place in the queue, from 0"),
            };
            answered(ask(shared, |reply| Call::Queue(edit, reply)))
        })),
        (Method::Post, ["v1", "queue", "move"]) => Some(needs(access.control, "control synththing").unwrap_or_else(|| {
            match (query.get("from").and_then(|v| v.parse().ok()), query.get("to").and_then(|v| v.parse().ok())) {
                (Some(from), Some(to)) => answered(ask(shared, |reply| Call::Queue(QueueEdit::Move { from, to }, reply))),
                _ => error(400, "say which song and where to, as places in the queue from 0: ?from=3&to=0"),
            }
        })),
        (Method::Get, ["v1", "now-playing"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::NowPlaying).map_or_else(|reply| reply, |now| json(200, &now))))
        }
        (Method::Get, ["v1", "songs"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::Library).map_or_else(|reply| reply, |library| songs(shared, &library, &query))))
        }
        (Method::Post, ["v1", "songs"]) => Some(match needs(access.add_songs, "add songs") {
            Some(reply) => reply,
            None => add_song(shared, &mut request, &access, &query),
        }),
        (Method::Get, ["v1", "staged"]) => Some(needs(access.add_songs, "add songs").unwrap_or_else(|| match ask(shared, Call::Library) {
            Ok(library) => json(200, &staged(shared).songs().map(|(key, song)| StagedInfo::of(key, song, &library)).collect::<Vec<_>>()),
            Err(reply) => reply,
        })),
        (Method::Post, ["v1", "staged"]) => Some(needs(access.add_songs, "add songs").unwrap_or_else(|| stage_song(shared, &mut request, &query))),
        (Method::Post, ["v1", "staged", "check"]) => Some(needs(access.add_songs, "add songs").unwrap_or_else(|| check_staged(shared, &query))),
        (Method::Post, ["v1", "staged", key, "import"]) => Some(needs(access.add_songs, "add songs").unwrap_or_else(|| import_staged(shared, &access, key))),
        (Method::Delete, ["v1", "staged", key]) => Some(needs(access.add_songs, "add songs").unwrap_or_else(|| {
            answered(Ok(if staged(shared).remove(key) { Ok(()) } else { Err((404, "there's no staged song with that key".to_string())) }))
        })),
        (Method::Get, ["v1", "songs", key]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            ask(shared, Call::Library).map_or_else(
                |reply| reply,
                |library| match library.store.get_by_key(key) {
                    Some(song) => json(200, &SongInfo::of(key, song, &library.store)),
                    None => error(404, "there's no song with that key"),
                },
            )
        })),
        (Method::Get, ["v1", "songs", key, "file"]) => match needs(access.download_songs, "download songs") {
            Some(reply) => Some(reply),
            None => match ask(shared, Call::Library) {
                Err(reply) => Some(reply),
                Ok(library) => match song_file(&library, key, request.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.as_str())) {
                    Err(reply) => Some(reply),
                    Ok(file) => {
                        let _ = request.respond(file);
                        return;
                    }
                },
            },
        },
        (Method::Get, ["v1", "songs", key, "art"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::Library).map_or_else(|reply| reply, |library| art(&library, key))))
        }
        (Method::Post | Method::Delete, ["v1", "songs", key, "star"]) => {
            let edit = Edit::Star { key: key.to_string(), on: method == Method::Post };
            Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| answered(ask(shared, |reply| Call::Edit(edit, reply)))))
        }
        (Method::Post | Method::Delete, ["v1", "songs", key, "tags"]) => {
            Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| match query.get("tag").map(|t| t.trim()).filter(|t| !t.is_empty()) {
                None => error(400, "say which tag: ?tag=<name>"),
                Some(tag) => {
                    let edit = Edit::Tag { key: key.to_string(), tag: tag.to_string(), on: method == Method::Post };
                    answered(ask(shared, |reply| Call::Edit(edit, reply)))
                }
            }))
        }
        (Method::Post, ["v1", "playlists"]) => Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| {
            match query.get("name").map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
                None => error(400, "say what to call it: ?name="),
                Some(name) => match ask(shared, |reply| Call::NewPlaylist(name, reply)) {
                    Ok(Ok(made)) => json(201, &made),
                    Ok(Err((status, why))) => error(status, why),
                    Err(reply) => reply,
                },
            }
        })),
        (Method::Delete, ["v1", "playlists", id]) => Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| {
            let edit = Edit::Delete { playlist: id.to_string() };
            answered(ask(shared, |reply| Call::Edit(edit, reply)))
        })),
        (Method::Post, ["v1", "playlists", id, "rename"]) => Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| {
            match query.get("to").map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
                None => error(400, "say the new name: ?to="),
                Some(name) => {
                    let edit = Edit::Rename { playlist: id.to_string(), name };
                    answered(ask(shared, |reply| Call::Edit(edit, reply)))
                }
            }
        })),
        (Method::Post, ["v1", "playlists", id, "songs", "move"]) => Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| {
            match (query.get("from").and_then(|v| v.parse().ok()), query.get("to").and_then(|v| v.parse().ok())) {
                (Some(from), Some(to)) => {
                    let edit = Edit::Move { playlist: id.to_string(), from, to };
                    answered(ask(shared, |reply| Call::Edit(edit, reply)))
                }
                _ => error(400, "say which song and where to, as places in the playlist from 0: ?from=3&to=0"),
            }
        })),
        (Method::Post | Method::Delete, ["v1", "playlists", id, "songs"]) => {
            Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| match query.get("song") {
                None => error(400, "say which song: ?song=<key>"),
                Some(key) => {
                    let edit = Edit::Playlist { playlist: id.to_string(), key: key.clone(), on: method == Method::Post };
                    answered(ask(shared, |reply| Call::Edit(edit, reply)))
                }
            }))
        }
        (Method::Get, ["v1", "tags"]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            ask(shared, Call::Library).map_or_else(
                |reply| reply,
                |library| {
                    let mut tags: Vec<_> = library
                        .store
                        .tags()
                        .values()
                        .map(|t| serde_json::json!({ "name": t.name, "color": format!("#{:02x}{:02x}{:02x}", t.color[0], t.color[1], t.color[2]) }))
                        .collect();
                    tags.sort_by_key(|t| t["name"].as_str().unwrap_or_default().to_lowercase());
                    json(200, &tags)
                },
            )
        })),
        (Method::Get, ["v1", "groupings"]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            ask(shared, Call::Library).map_or_else(
                |reply| reply,
                |library| {
                    let list: Vec<_> =
                        library.rulesets.iter().map(|(id, r)| GroupingInfo { id, name: &r.name, split: r.rules.split.as_ref().map(|s| s.by) }).collect();
                    json(200, &list)
                },
            )
        })),
        (Method::Get, ["v1", "groupings", id]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            ask(shared, Call::Library).map_or_else(
                |reply| reply,
                |library| match library.rulesets.iter().find(|(i, _)| i == id) {
                    Some((id, ruleset)) => {
                        let groups = library.groups(&ruleset.rules);
                        let info = GroupingInfo { id, name: &ruleset.name, split: ruleset.rules.split.as_ref().map(|s| s.by) };
                        json(200, &serde_json::json!({ "id": info.id, "name": info.name, "split": info.split, "groups": groups }))
                    }
                    None => error(404, format!("there's no grouping \"{id}\"")),
                },
            )
        })),
        (Method::Get, ["v1", "groups"]) => Some(needs(access.read, "read").unwrap_or_else(|| match split_by(&query) {
            Err(reply) => reply,
            Ok(by) => ask(shared, Call::Library).map_or_else(|reply| reply, |library| groups(&library, by, query.get("q").map_or("", String::as_str))),
        })),
        (Method::Get, ["v1", "wishlist"]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            let q = query.get("q").cloned().unwrap_or_default();
            ask(shared, |reply| Call::Wishlist(q, reply)).map_or_else(|reply| reply, |wishes| json(200, &wishes))
        })),
        (Method::Post, ["v1", "wishlist"]) => Some(match needs(access.edit_library, "edit the library") {
            Some(reply) => reply,
            None => wish_for(shared, &mut request, &access),
        }),
        (Method::Delete, ["v1", "wishlist", id]) => Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| {
            let edit = Edit::Unwish { id: id.to_string() };
            answered(ask(shared, |reply| Call::Edit(edit, reply)))
        })),
        (Method::Post, ["v1", "wishlist", id, "note"]) => Some(needs(access.edit_library, "edit the library").unwrap_or_else(|| {
            let edit = Edit::Note { id: id.to_string(), note: query.get("text").map(|t| t.trim().to_string()).unwrap_or_default() };
            answered(ask(shared, |reply| Call::Edit(edit, reply)))
        })),
        (Method::Get, ["v1", "musicbrainz", "search"]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            let q = query.get("q").map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
            match (q, query.get("kind").map(String::as_str)) {
                (None, _) => error(400, "say what to search for: ?q="),
                (_, Some(kind)) if !["artist", "album", "song"].contains(&kind) => error(400, "?kind= is artist, album or song"),
                (Some(q), kind) => {
                    let q = if query.get("advanced").is_some_and(|a| a == "true") { Query::Advanced(q) } else { Query::Words(q) };
                    online(shared).unwrap_or_else(|| musicbrainz_search(&q, kind))
                }
            }
        })),
        (Method::Get, ["v1", "musicbrainz", "artists", id, "albums"]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            online(shared).unwrap_or_else(|| match MusicBrainz::new().discography(id) {
                Ok(albums) => {
                    // (Each as the wish for it would be, under none yet.)
                    let wishes: Vec<Wish> = albums.iter().map(|a| Wish { under: None, artist: None, ..crate::wishlist::album_wish(a, "", "") }).collect();
                    json(200, &wishes)
                }
                Err(e) => error(502, format!("couldn't ask MusicBrainz: {e:#}")),
            })
        })),
        (Method::Get, ["v1", "musicbrainz", "releases", id]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            // (Kept releases answer at once; others wait their turn to ask.)
            online(shared).unwrap_or_else(|| match crate::lookup::musicbrainz::kept_release(id).map_or_else(|| MusicBrainz::new().fresh_release(id), Ok) {
                Ok(release) => json(200, &ReleaseInfo::from(release)),
                Err(e) => error(502, format!("couldn't ask MusicBrainz: {e:#}")),
            })
        })),
        (Method::Get, ["v1", "soundfonts"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::Library).map_or_else(|reply| reply, |library| json(200, &library.soundfonts))))
        }
        (Method::Post, ["v1", "soundfonts", id, "use"]) => {
            let id = id.to_string();
            Some(needs(access.control, "control synththing").unwrap_or_else(|| answered(ask(shared, |reply| Call::UseSoundfont(id, reply)))))
        }
        (Method::Get, ["v1", "scripts"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::Library).map_or_else(|reply| reply, |library| json(200, &library.scripts))))
        }
        (Method::Post, ["v1", "scripts", id, "use"]) => {
            let id = id.to_string();
            Some(needs(access.control, "control synththing").unwrap_or_else(|| answered(ask(shared, |reply| Call::UseScript(id, reply)))))
        }
        (Method::Get, ["v1", "playlists"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::Library).map_or_else(|reply| reply, |library| json(200, &library.playlists))))
        }
        _ => None,
    };
    let _ = request.respond(reply.unwrap_or_else(|| error(404, "no such request (see docs/API.md)")));
}

/// `POST /v1/songs?name=<file name>`, the file as the body: into the
/// store, then the library (`Call::Add`). 201 with the song if it's new to
/// the library, 200 if it was there already.
fn add_song(shared: &Shared, request: &mut Request, access: &Access, query: &HashMap<String, String>) -> Reply {
    let (name, bytes) = match song_sent(request, query) {
        Ok(sent) => sent,
        Err(reply) => return reply,
    };
    let stored = match crate::config::music_dir().and_then(|dir| song_store::write_in(&bytes, &name, &dir)) {
        Ok(stored) => stored,
        Err(e) => return error(500, format!("couldn't store the song: {e:#}")),
    };
    match ask(shared, |reply| Call::Add { stored, program: access.name.clone(), online: None, reply }) {
        Ok(Ok((song, new))) => json(if new { 201 } else { 200 }, &song),
        Ok(Err(why)) => error(503, why),
        Err(reply) => reply,
    }
}

/// The song file sent as the body, named `?name=`: its name and bytes, if
/// it's a file synththing plays and not too big.
fn song_sent(request: &mut Request, query: &HashMap<String, String>) -> Result<(String, Vec<u8>), Reply> {
    let Some(name) = query.get("name").map(|n| n.trim()).filter(|n| !n.is_empty() && !n.contains(['/', '\\'])) else {
        return Err(error(400, "say the song's file name: ?name=Song.mp3"));
    };
    if !song_store::takes(std::path::Path::new(name), crate::app::SONG_EXTENSIONS) {
        return Err(error(415, format!("{name} isn't a file synththing can play")));
    }
    if request.body_length().is_some_and(|n| n > LARGEST_SONG) {
        return Err(error(413, "that song is too big (250 MB at most)"));
    }
    let mut bytes = Vec::new();
    match request.as_reader().take(LARGEST_SONG as u64 + 1).read_to_end(&mut bytes) {
        Ok(_) if bytes.len() > LARGEST_SONG => Err(error(413, "that song is too big (250 MB at most)")),
        Ok(_) if bytes.is_empty() => Err(error(400, "send the song's file as the body")),
        Ok(_) => Ok((name.to_string(), bytes)),
        Err(e) => Err(error(400, format!("couldn't read the song: {e}"))),
    }
}

/// A staged song as the API gives it: as a song of the library would be
/// (its match at MusicBrainz too, once it's checked), whether the library
/// has it already, the wishes it would fill (by ID), and the library's
/// other copies of it (`duplicates::copies_of`), with whether it's better
/// than all of them.
#[derive(Serialize)]
struct StagedInfo {
    #[serde(flatten)]
    song: SongInfo,
    here: bool,
    fills: Vec<String>,
    duplicates: Vec<Duplicate>,
    better: bool,
}

#[derive(Serialize)]
struct Duplicate {
    key: String,
    /// One recording at MusicBrainz, not guessed from title, artist and length.
    sure: bool,
}

impl StagedInfo {
    fn of(key: &str, song: &Song, library: &Library) -> Self {
        let fills = crate::wishlist::Index::new(&library.wishes).fills(song).into_iter().map(str::to_string).collect();
        let here = library.store.has(key);
        let (copies, better) = if here { (Vec::new(), false) } else { crate::duplicates::copies_of(&library.store, song, &library.group_ignore) };
        let duplicates = copies.into_iter().map(|(key, sure)| Duplicate { key, sure }).collect();
        StagedInfo { song: SongInfo::of(key, song, &library.store), here, fills, duplicates, better }
    }
}

/// What checking an album of staged songs at MusicBrainz made of it:
/// `matched` (to `release`), `unsure` (one of `candidates`, maybe),
/// `not_found` or `failed` (`error`), and its songs as they are now.
#[derive(Serialize)]
struct Checked {
    album: String,
    artist: Option<String>,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    release: Option<ReleaseInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<ReleaseInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    songs: Vec<StagedInfo>,
}

fn staged(shared: &Shared) -> std::sync::MutexGuard<'_, Staged> {
    shared.staged.lock().unwrap_or_else(|e| e.into_inner())
}

/// `POST /v1/staged?name=`, the file as the body: staged and read, not in
/// the library yet. 201 with it if it's new, 200 if it was staged already.
fn stage_song(shared: &Shared, request: &mut Request, query: &HashMap<String, String>) -> Reply {
    let (name, bytes) = match song_sent(request, query) {
        Ok(sent) => sent,
        Err(reply) => return reply,
    };
    let library = match ask(shared, Call::Library) {
        Ok(library) => library,
        Err(reply) => return reply,
    };
    let mut staged = staged(shared);
    match staged.add(&bytes, &name) {
        Ok((key, new)) => {
            let song = staged.get(&key).expect("just staged");
            json(if new { 201 } else { 200 }, &StagedInfo::of(&key, song, &library))
        }
        Err(e) => error(422, format!("{e:#}")),
    }
}

/// `POST /v1/staged/check?keys=`: the staged songs `keys` (comma
/// separated; else all of them) matched at MusicBrainz, album by album, as
/// the library's songs are; each song keeps its match for when it's
/// imported. Waits for MusicBrainz (what it's answered before is kept).
fn check_staged(shared: &Shared, query: &HashMap<String, String>) -> Reply {
    let library = match ask(shared, Call::Library) {
        Ok(library) if !library.online => return error(409, "asking MusicBrainz needs Online album art and details on (Preferences > Library)"),
        Ok(library) => library,
        Err(reply) => return reply,
    };
    let wanted: Option<Vec<&str>> = query.get("keys").map(|keys| keys.split(',').map(str::trim).filter(|k| !k.is_empty()).collect());
    let songs: Vec<(String, Song)> = {
        let mut staged = staged(shared);
        let songs: Vec<(String, Song)> =
            staged.songs().filter(|(key, _)| wanted.as_ref().is_none_or(|w| w.contains(key))).map(|(key, song)| (key.to_string(), song.clone())).collect();
        if let Some(missing) = wanted.iter().flatten().find(|key| !songs.iter().any(|(k, _)| k == *key)) {
            return error(404, format!("{missing} isn't staged"));
        }
        songs
    };
    // (Asked without holding the staged songs: other requests go on.)
    let albums = crate::lookup::albums_of(songs.iter().map(|(key, song)| (key.as_str(), song)), &library.group_ignore);
    let mut musicbrainz = MusicBrainz::new();
    let answers: Vec<_> = albums.into_iter().map(|album| (crate::lookup::look_up(&mut musicbrainz, &album, false), album)).collect();
    let mut staged = staged(shared);
    let checked: Vec<Checked> = answers
        .into_iter()
        .map(|(outcome, album)| {
            let (mut release, mut candidates, mut error) = (None, Vec::new(), None);
            let outcome = match outcome {
                crate::lookup::Outcome::Matched(found, tracks) => {
                    for (key, online) in crate::lookup::matches(&album, &found, &tracks, false, &library.store) {
                        staged.matched(&key, online);
                    }
                    release = Some(ReleaseInfo::from(*found));
                    "matched"
                }
                crate::lookup::Outcome::Unsure(found) => {
                    candidates = found.into_iter().map(ReleaseInfo::from).collect();
                    "unsure"
                }
                crate::lookup::Outcome::NotFound => "not_found",
                crate::lookup::Outcome::Failed(why) => {
                    error = Some(why);
                    "failed"
                }
            };
            let songs = album.songs.iter().filter_map(|s| Some(StagedInfo::of(&s.key, staged.get(&s.key)?, &library))).collect();
            Checked { album: album.name, artist: album.artist, outcome, release, candidates, error, songs }
        })
        .collect();
    json(200, &checked)
}

/// `POST /v1/staged/{key}/import`: the staged song into the library, with
/// its match if it was checked, without being sent again.
fn import_staged(shared: &Shared, access: &Access, key: &str) -> Reply {
    let dir = match crate::config::music_dir() {
        Ok(dir) => dir,
        Err(e) => return error(500, format!("couldn't find the library: {e:#}")),
    };
    let (stored, online) = match staged(shared).import(key, &dir) {
        Ok(Some(imported)) => imported,
        Ok(None) => return error(404, "there's no staged song with that key"),
        Err(e) => return error(500, format!("couldn't import it: {e:#}")),
    };
    match ask(shared, |reply| Call::Add { stored, program: access.name.clone(), online, reply }) {
        Ok(Ok((song, new))) => json(if new { 201 } else { 200 }, &song),
        Ok(Err(why)) => error(503, why),
        Err(reply) => reply,
    }
}

/// `None` if MusicBrainz may be asked from here (Online album art and
/// details on), else why not.
fn online(shared: &Shared) -> Option<Reply> {
    match ask(shared, Call::Library) {
        Err(reply) => Some(reply),
        Ok(library) if !library.online => Some(error(409, "asking MusicBrainz needs Online album art and details on (Preferences > Library)")),
        Ok(_) => None,
    }
}

/// `GET /v1/musicbrainz/search?q=&kind=`: the artists, albums and songs
/// (or only those of `kind`) MusicBrainz finds, each as the wish for it.
fn musicbrainz_search(q: &Query, kind: Option<&str>) -> Reply {
    let mut musicbrainz = MusicBrainz::new();
    let found = (|| {
        let mut found = serde_json::Map::new();
        if kind.is_none_or(|k| k == "artist") {
            found.insert("artists".into(), serde_json::to_value(musicbrainz.find_artists(q)?)?);
        }
        if kind.is_none_or(|k| k == "album") {
            found.insert("albums".into(), serde_json::to_value(musicbrainz.find_albums(q)?)?);
        }
        if kind.is_none_or(|k| k == "song") {
            found.insert("songs".into(), serde_json::to_value(musicbrainz.find_songs(q)?)?);
        }
        Ok::<_, anyhow::Error>(found)
    })();
    match found {
        Ok(found) => json(200, &found),
        Err(e) => error(502, format!("couldn't ask MusicBrainz: {e:#}")),
    }
}

/// `POST /v1/wishlist`, the wish as a JSON body (as `/v1/musicbrainz/search`
/// gives it, or written out): onto the wishlist (`Call::Wish`). An album
/// by its MusicBrainz group without its tracks gets them from there
/// first, when it may be asked. 201 with the wish if it's new, 200 if it
/// was wished for already.
fn wish_for(shared: &Shared, request: &mut Request, access: &Access) -> Reply {
    let mut body = Vec::new();
    if let Err(e) = request.as_reader().take(64 * 1024).read_to_end(&mut body) {
        return error(400, format!("couldn't read the wish: {e}"));
    }
    let mut wish: Wish = match serde_json::from_slice(&body) {
        Ok(wish) => wish,
        Err(e) => return error(400, format!("send the wish as JSON, like {{\"kind\": \"album\", \"title\": \"Modal Soul\", \"artist\": \"Nujabes\"}}: {e}")),
    };
    wish.title = wish.title.trim().to_string();
    if wish.title.is_empty() && !(wish.kind == Kind::Text && wish.artist.is_some()) {
        return error(400, "say what it's called: \"title\" (or for a wish by name, at least \"artist\")");
    }
    let mut release = None;
    if let (Kind::Album, Some(group), 0) = (wish.kind, wish.musicbrainz.clone(), wish.tracks)
        && online(shared).is_none()
    {
        match MusicBrainz::new().album(&group) {
            // (As it was asked for, its tracks as MusicBrainz says.)
            Ok((found, edition)) => {
                let asked = std::mem::replace(&mut wish, found);
                wish.title = asked.title;
                wish.artist = asked.artist.or(wish.artist.take());
                wish.year = asked.year.or(wish.year.take());
                wish.release_type = asked.release_type.or(wish.release_type.take());
                (wish.under, wish.note, wish.skipped) = (asked.under, asked.note, asked.skipped);
                release = Some(edition);
            }
            Err(e) => return error(502, format!("couldn't ask MusicBrainz about that album: {e:#}")),
        }
    }
    match ask(shared, |reply| Call::Wish { wish, release, program: access.name.clone(), reply }) {
        Ok(Ok((wish, new))) => json(if new { 201 } else { 200 }, &wish),
        Ok(Err((status, why))) => error(status, why),
        Err(reply) => reply,
    }
}

#[derive(Serialize)]
struct SongPage {
    /// Songs the query matches, in all (with `since`: of those changed).
    total: usize,
    songs: Vec<SongInfo>,
    /// The library's revision now: `?since=` it next time for only what
    /// changes after.
    revision: String,
    /// Whether this is every song the query matches, not only those
    /// changed (no `since`, or one from before synththing last started).
    full: bool,
    /// With `since`: the songs no longer in the list (gone from the
    /// library, or changed so the query doesn't find them).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    removed: Vec<String>,
}

/// `GET /v1/songs?q=&offset=&limit=&since=`: the songs the query matches
/// (the Songs search's syntax), by file name, a page of them; with
/// `since` (a revision an earlier answer gave), only those that changed
/// after it, and the ones that left the list.
fn songs(shared: &Shared, library: &Library, query: &HashMap<String, String>) -> Reply {
    let store = &library.store;
    let q = query.get("q").map_or("", String::as_str);
    let offset = query.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
    let limit = query.get("limit").and_then(|l| l.parse().ok()).unwrap_or(MOST_SONGS).min(MOST_SONGS);
    let mut changes = shared.changes.lock().unwrap_or_else(|e| e.into_inner());
    changes.update(store);
    let since = query.get("since").and_then(|since| changes.since(since));
    let mut found = matching(store, &library.wishes, q);
    let mut removed = Vec::new();
    if let Some(since) = since {
        found.retain(|(key, _)| changes.changed_after(key, since));
        let kept: std::collections::HashSet<&str> = found.iter().map(|(key, _)| key.as_str()).collect();
        removed = changes.gone.iter().filter(|(_, rev)| **rev > since).map(|(key, _)| key.clone()).collect();
        removed.extend(changes.seen.iter().filter(|(key, (_, rev))| *rev > since && !kept.contains(key.as_str())).map(|(key, _)| key.clone()));
        removed.sort();
    }
    let total = found.len();
    let songs = found.into_iter().skip(offset).take(limit).map(|(key, song)| SongInfo::of(&key, song, store)).collect();
    json(200, &SongPage { total, songs, revision: changes.revision(), full: since.is_none(), removed })
}

/// `?split=` as what to split by: `artist`, `album_artist`, `album`,
/// `composer`, `genre`, `year` or `tag`.
fn split_by(query: &HashMap<String, String>) -> Result<SplitBy, Reply> {
    let named = query.get("split").ok_or_else(|| error(400, "say what to split by: ?split=album"))?;
    serde_json::from_value(serde_json::Value::String(named.clone()))
        .map_err(|_| error(400, format!("can't split by \"{named}\": artist, album_artist, album, composer, genre, year or tag")))
}

/// `GET /v1/groups?split=&q=`: the songs `q` finds (as `/v1/songs` does),
/// split as a ruleset splitting by `by` would, in album order in each.
fn groups(library: &Library, by: SplitBy, q: &str) -> Reply {
    let found: std::collections::HashSet<String> = matching(&library.store, &library.wishes, q).into_iter().map(|(key, _)| key).collect();
    let rules = Rules { split: Some(Split { by, min: 1 }), order: Order::Album, seed: 0, ..Rules::default() };
    let groups: Vec<GroupInfo> = library
        .groups(&rules)
        .into_iter()
        .filter_map(|mut group| {
            group.songs.retain(|key| found.contains(key));
            (!group.songs.is_empty()).then_some(group)
        })
        .collect();
    json(200, &groups)
}

/// `GET /v1/songs/{key}/file`: the song's file as it's kept, named as it
/// was imported.
/// `GET /v1/songs/{key}/file`: the whole file, or the part `range` (the
/// request's `Range` header) asks for, so players can seek in it.
fn song_file(library: &Library, key: &str, range: Option<&str>) -> Result<Response<std::io::Take<std::fs::File>>, Reply> {
    use std::io::Seek;
    let song = library.store.get_by_key(key).filter(|_| library.store.has(key)).ok_or_else(|| error(404, "there's no song with that key here"))?;
    let path = library.store.path(key);
    let mut file = std::fs::File::open(&path).map_err(|e| error(500, format!("couldn't read the song: {e}")))?;
    let length = file.metadata().map_err(|e| error(500, format!("couldn't read the song: {e}")))?.len();
    let part = match range.map(|r| byte_range(r, length)) {
        None | Some(Ok(None)) => None,
        Some(Ok(Some(part))) => Some(part),
        Some(Err(())) => {
            return Err(error(416, "that range isn't in the file").with_header(header("Content-Range", &format!("bytes */{length}"))));
        }
    };
    let (status, start, end) = match part {
        Some((start, end)) => (206, start, end),
        None => (200, 0, length.saturating_sub(1)),
    };
    file.seek(std::io::SeekFrom::Start(start)).map_err(|e| error(500, format!("couldn't read the song: {e}")))?;
    let size = if length == 0 { 0 } else { end - start + 1 };
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let kind = match extension.as_str() {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" | "mp4" | "aac" => "audio/mp4",
        "mid" | "midi" => "audio/midi",
        _ => "application/octet-stream",
    };
    // (RFC 6266's filename*, for names past ASCII, after a plain one for
    // clients that only read that.)
    let plain: String = song.name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    let encoded: String =
        song.name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    // Never chunked, so the length is sent and clients can show progress.
    let mut response = Response::new(tiny_http::StatusCode(status), Vec::new(), file.take(size), Some(size as usize), None)
        .with_chunked_threshold(usize::MAX)
        .with_header(header("Accept-Ranges", "bytes"))
        .with_header(header("Content-Type", kind));
    if status == 206 {
        response.add_header(header("Content-Range", &format!("bytes {start}-{end}/{length}")));
    }
    Ok(response
        .with_header(header("Content-Disposition", &format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}"))))
}

/// A `Range: bytes=...` header's one range, in a file `length` bytes long:
/// the first and last byte, `None` to send all of it (no `bytes=` range, or
/// several, which are allowed to be answered with the whole file), or `Err`
/// if it's past the end.
fn byte_range(header: &str, length: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = header.trim().strip_prefix("bytes=") else { return Ok(None) };
    if spec.contains(',') {
        return Ok(None);
    }
    let Some((first, last)) = spec.trim().split_once('-') else { return Ok(None) };
    let (first, last) = (first.trim(), last.trim());
    let (start, end) = match (first.parse::<u64>(), last.parse::<u64>()) {
        // The last `n` bytes.
        (Err(_), Ok(n)) if first.is_empty() => (length.saturating_sub(n), length.saturating_sub(1)),
        (Ok(start), Err(_)) if last.is_empty() => (start, length.saturating_sub(1)),
        (Ok(start), Ok(end)) if start <= end => (start, end.min(length.saturating_sub(1))),
        _ => return Ok(None),
    };
    if length == 0 || start >= length || (first.is_empty() && last == "0") {
        return Err(());
    }
    Ok(Some((start, end)))
}

/// `GET /v1/songs/{key}/art`: the song's picture (its own, else its
/// release's cover), as the app would show it for its album.
fn art(library: &Library, key: &str) -> Reply {
    let Some(song) = library.store.get_by_key(key) else { return error(404, "there's no song with that key") };
    let source = crate::album_art::Source::of_song(library.store.has(key).then(|| library.store.path(key)), Some(song));
    match crate::album_art::picture(&source, library.online) {
        Some(bytes) => {
            let kind = match image::guess_format(&bytes) {
                Ok(image::ImageFormat::Png) => "image/png",
                Ok(image::ImageFormat::Gif) => "image/gif",
                Ok(image::ImageFormat::WebP) => "image/webp",
                _ => "image/jpeg",
            };
            Response::from_data(bytes.into_vec()).with_header(header("Content-Type", kind))
        }
        None => error(404, "that song has no art"),
    }
}

/// The library's songs that `query` keeps: its filters, then each word of
/// text found in the title, artist, album or file name.
fn matching<'s>(store: &'s SongStore, wishes: &crate::wishlist::Wishes, query: &str) -> Vec<(String, &'s Song)> {
    let search = crate::search::parse(query, store.tags());
    let rules = search.rules.matcher(store, wishes, crate::settings_sync::now());
    let mut found: Vec<(String, &Song)> = store
        .songs()
        .filter(|(path, song)| crate::song_store::key_in(store.dir(), path).is_some_and(|key| rules(key, song)))
        .filter(|(_, song)| {
            let facts = Facts::of(song);
            let hay: Vec<String> = [Field::Title, Field::Artist, Field::Album]
                .into_iter()
                .filter_map(|f| facts.text(f))
                .map(|t| t.into_owned())
                .chain([song.name.clone()])
                .collect();
            search.text.iter().all(|term| hay.iter().any(|h| term.find(h).is_some()))
        })
        .filter_map(|(path, song)| Some((song_store::key_in(store.dir(), &path)?.to_string(), song)))
        .collect();
    found.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    found
}

/// `GET /v1/events`: keep the connection, on a thread of its own, and send
/// it every event until it's closed.
/// A page OBS's Browser Source, or a browser, can show: the visualizer's
/// stream and/or a player for what plays, with the page's own query (token
/// and all) passed on to them. The pictures are PNG unless it asks for
/// JPEG: no artifacts on pixel art, see-through parts kept, and small at
/// the sizes a page shows. Scaled up, they stay pixelated.
fn page(title: &str, raw_query: &str, video: bool, audio: bool) -> Reply {
    let query = raw_query.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    let mut page = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>synththing {title}</title>\
         <style>html,body{{margin:0;height:100%;background:transparent;overflow:hidden}}\
         img{{width:100%;height:100%;object-fit:contain;display:block;image-rendering:pixelated}}\
         button{{position:fixed;inset:0;margin:auto;width:max-content;height:max-content;padding:.6em 1.2em;font:16px sans-serif;cursor:pointer}}</style>"
    );
    if video {
        let png = if raw_query.split('&').any(|p| p.starts_with("format=")) { "" } else { "&amp;format=png" };
        page += &format!("<img src=\"/v1/visualizer/stream?{query}{png}\" alt=\"\">");
    }
    if audio {
        // (A JSON string, as script text isn't HTML: `&amp;` would stay
        // as it is. `<` escaped so the query can't end the script.)
        let stream = serde_json::to_string(&format!("/v1/audio/stream?{raw_query}")).unwrap_or_default().replace('<', "\\u003c");
        page += &format!("<button hidden>Click to listen</button><script>const STREAM = {stream};\n{PLAYER}</script>");
    }
    Response::from_data(page)
        .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
}

/// Plays `STREAM` (`/v1/audio/stream`, whose 16-bit stereo it reads past
/// the WAV header) a little behind as it comes, so it keeps up with the
/// visualizer. Each piece is scheduled right after the last: if that's
/// fallen behind, or drifted too far ahead, it starts over `LEAD` ahead.
/// A browser only lets a page make sound after a click, so until then it
/// shows a button; OBS lets it straight away.
const PLAYER: &str = r#"
const LEAD = 0.15, RATE = 44100;
const ctx = new AudioContext({ sampleRate: RATE, latencyHint: "interactive" });
const button = document.querySelector("button");
const shown = () => { button.hidden = ctx.state === "running"; };
ctx.onstatechange = shown;
button.onclick = () => ctx.resume();
shown();
let next = 0;
function play(bytes) {
  const frames = bytes.length >> 2;
  if (!frames || ctx.state !== "running") return;
  const view = new DataView(bytes.buffer, bytes.byteOffset, frames * 4);
  const buffer = ctx.createBuffer(2, frames, RATE);
  const left = buffer.getChannelData(0), right = buffer.getChannelData(1);
  for (let i = 0; i < frames; i++) {
    left[i] = view.getInt16(i * 4, true) / 32768;
    right[i] = view.getInt16(i * 4 + 2, true) / 32768;
  }
  const now = ctx.currentTime;
  if (next < now || next > now + LEAD + 0.25) next = now + LEAD;
  const source = ctx.createBufferSource();
  source.buffer = buffer;
  source.connect(ctx.destination);
  source.start(next);
  next += buffer.duration;
}
async function listen() {
  try {
    const reader = (await fetch(STREAM, { cache: "no-store" })).body.getReader();
    let header = 44, rest = new Uint8Array(0);
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      let bytes = new Uint8Array(rest.length + value.length);
      bytes.set(rest);
      bytes.set(value, rest.length);
      const skipped = Math.min(header, bytes.length);
      bytes = bytes.subarray(skipped);
      header -= skipped;
      const whole = bytes.length - (bytes.length % 4);
      rest = bytes.slice(whole);
      play(bytes.subarray(0, whole));
    }
  } catch (e) {}
  setTimeout(listen, 1000);
}
listen();
"#;

/// Send what plays to `request`, as a WAV that doesn't end, until it goes.
fn listen(request: Request, chunks: Receiver<crate::audio_stream::Chunk>) {
    let spawned = std::thread::Builder::new().name("api listener".into()).spawn(move || {
        let mut writer = request.into_writer();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
        let mut ok = writer
            .write_all(head.as_bytes())
            .and_then(|()| writer.write_all(&crate::audio_stream::wav_header(SAMPLE_RATE)))
            .and_then(|()| writer.flush())
            .is_ok();
        while ok {
            // (Nothing for a while: no audio's going out. The player's kept
            // going, and the connection checked, with that much silence.)
            let chunk = chunks.recv_timeout(QUIET).unwrap_or_else(|_| vec![0u8; SAMPLE_RATE as usize / 4 * 4].into());
            ok = writer.write_all(&chunk).and_then(|()| writer.flush()).is_ok();
        }
    });
    if spawned.is_err() {
        log::warn!("api: couldn't start an audio stream");
    }
}

/// `?width=&height=&fps=` for the visualizer's pictures: the size to draw
/// them at (both or neither), and how many a second at most.
fn watch_options(query: &HashMap<String, String>) -> Result<(Option<(usize, usize)>, u32, Format), Reply> {
    let side = |name: &str| query.get(name).map(|v| v.parse::<u32>().map_err(|_| error(400, format!("?{name}= is a number of pixels"))));
    let size = match (side("width").transpose()?, side("height").transpose()?) {
        (Some(w), Some(h)) => Some(crate::recorder::Resolution::custom(w, h).size()),
        (None, None) => None,
        _ => return Err(error(400, "give both ?width= and ?height=, or neither")),
    };
    let fps = match query.get("fps") {
        None => DEFAULT_FPS,
        Some(fps) => fps.parse::<u32>().ok().filter(|f| (1..=60).contains(f)).ok_or_else(|| error(400, "?fps= is 1 to 60"))?,
    };
    let format = match query.get("format").map(String::as_str) {
        None | Some("jpeg") => Format::Jpeg,
        Some("png") => Format::Png,
        Some(_) => return Err(error(400, "?format= is jpeg or png")),
    };
    Ok((size, fps, format))
}

/// Send the visualizer's pictures to `request` as they're drawn, in
/// `format`, as `multipart/x-mixed-replace` (MJPEG, for JPEG), until it
/// goes.
fn video(shared: &Arc<Shared>, request: Request, size: Option<(usize, usize)>, fps: u32, format: Format) {
    let shared = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("api visualizer".into()).spawn(move || {
        let watching = shared.frames.watch(size, format);
        shared.waker.request_repaint();
        let mut writer = request.into_writer();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-store\r\n\
                    Connection: close\r\n\r\n";
        let mut ok = writer.write_all(head.as_bytes()).and_then(|()| writer.flush()).is_ok();
        let (mut seen, gap) = (shared.frames.count(), Duration::from_secs_f64(1.0 / fps as f64));
        while ok {
            let started = std::time::Instant::now();
            // (Nothing new for a while: the visualizer isn't drawing. The
            // connection's checked by sending the last picture again.)
            let (picture, count) = match shared.frames.next(seen, KEEP_ALIVE, format) {
                Some(frame) => frame,
                None => match shared.frames.next(seen.saturating_sub(1), Duration::ZERO, format) {
                    Some(frame) => frame,
                    None => continue,
                },
            };
            seen = count;
            let part = format!(
                "--frame\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
                format.content_type(),
                picture.len()
            );
            ok = writer
                .write_all(part.as_bytes())
                .and_then(|()| writer.write_all(&picture))
                .and_then(|()| writer.write_all(b"\r\n"))
                .and_then(|()| writer.flush())
                .is_ok();
            std::thread::sleep(gap.saturating_sub(started.elapsed()));
        }
        drop(watching);
    });
    if spawned.is_err() {
        log::warn!("api: couldn't start a visualizer stream");
    }
}

/// A panel's JSON sent as the body, if it's no bigger than a panel may be.
fn panel_json(request: &mut Request) -> Result<serde_json::Value, Reply> {
    let mut body = Vec::new();
    if request.as_reader().take(crate::panel::LARGEST as u64 + 1).read_to_end(&mut body).is_err() {
        return Err(error(400, "couldn't read the request"));
    }
    if body.len() > crate::panel::LARGEST {
        return Err(error(413, format!("a panel is at most {} KB", crate::panel::LARGEST >> 10)));
    }
    serde_json::from_slice(&body).map_err(|e| error(400, format!("not JSON: {e}")))
}

/// A part of the request's path, percent-decoded.
fn path_part(part: &str) -> String {
    percent_encoding::percent_decode_str(part).decode_utf8_lossy().into_owned()
}

/// `PUT /v1/shared/{key}`: shares the JSON sent as the body under `key`
/// (`null` takes it out). 204.
fn share(shared: &Shared, request: &mut Request, key: &str) -> Reply {
    let mut body = Vec::new();
    if request.as_reader().take(crate::shared_data::LARGEST as u64 + 1).read_to_end(&mut body).is_err() {
        return error(400, "couldn't read the request");
    }
    if body.len() > crate::shared_data::LARGEST {
        return error(413, format!("shared data holds at most {} KB", crate::shared_data::LARGEST >> 10));
    }
    let value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => return error(400, format!("not JSON: {e}")),
    };
    match crate::shared_data::set(key, Some(value)) {
        Ok(changed) => {
            if changed {
                shared.waker.request_repaint();
            }
            Response::from_data(Vec::new()).with_status_code(204)
        }
        Err(e) => error(413, e),
    }
}

/// `PUT /v1/panel`: the program's panel, as sent, in place of any it had.
fn set_panel(shared: &Shared, request: &mut Request, access: &Access) -> Reply {
    let source = match panel_json(request) {
        Ok(source) => source,
        Err(reply) => return reply,
    };
    let panel = match Panel::parse(&source) {
        Ok(panel) => panel,
        Err(why) => return error(400, why),
    };
    let mut panels = lock(&shared.panels);
    panels.revision += 1;
    let revision = panels.revision;
    let values = panel.values();
    let shown = match panels.shown.iter().position(|s| s.app == access.token) {
        Some(at) => &mut panels.shown[at],
        None => {
            let (app, name, share) = (access.token.clone(), access.name.clone(), access.share);
            panels.shown.push(ShownPanel { app, name, panel: panel.clone(), given: Vec::new(), source: source.clone(), share });
            panels.shown.last_mut().expect("just pushed")
        }
    };
    (shown.name, shown.panel, shown.source) = (access.name.clone(), panel, source);
    shown.give(values, revision);
    shared.waker.request_repaint();
    Response::from_data(Vec::new()).with_status_code(204)
}

/// `PATCH /v1/panel/elements/{id}`: some of one element's fields changed.
fn patch_panel(shared: &Shared, request: &mut Request, access: &Access, id: &str) -> Reply {
    let changes = match panel_json(request) {
        Ok(changes) => changes,
        Err(reply) => return reply,
    };
    let value = changes.get("value").and_then(|v| v.as_str()).map(str::to_string);
    let mut panels = lock(&shared.panels);
    let Some(shown) = panels.shown.iter_mut().find(|s| s.app == access.token) else { return error(404, "you have no panel: PUT /v1/panel first") };
    let mut source = shown.source.clone();
    if let Err((status, why)) = crate::panel::patch(&mut source, id, changes) {
        return error(status, why);
    }
    match Panel::parse(&source) {
        Ok(panel) => (shown.panel, shown.source) = (panel, source),
        Err(why) => return error(400, why),
    }
    let revision = panels.revision + 1;
    let shown = panels.shown.iter_mut().find(|s| s.app == access.token).expect("found above");
    shown.give(value.map(|value| (id.to_string(), value)).into_iter().collect(), revision);
    panels.revision = revision;
    shared.waker.request_repaint();
    Response::from_data(Vec::new()).with_status_code(204)
}

fn stream(shared: &Arc<Shared>, request: Request, app: String, wanted: Option<Vec<String>>) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<String>();
    {
        let mut streams = lock(&shared.streams);
        if streams.len() >= MOST_STREAMS {
            drop(streams);
            let _ = request.respond(error(503, "too many open event streams"));
            return;
        }
        streams.push(Stream { id, app: app.clone(), wanted, tx });
    }
    // (A program's panel shows whether it's listening.)
    shared.waker.request_repaint();
    let closing = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("api events".into()).spawn(move || {
        let mut writer = request.into_writer();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
                    Connection: close\r\n\r\n: open\n\n";
        let mut ok = writer.write_all(head.as_bytes()).and_then(|()| writer.flush()).is_ok();
        while ok {
            let has_panel = lock(&closing.panels).shown.iter().any(|shown| shown.app == app);
            let chunk = match rx.recv_timeout(if has_panel { PANEL_KEEP_ALIVE } else { KEEP_ALIVE }) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => ": keep-alive\n\n".to_string(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            ok = writer.write_all(chunk.as_bytes()).and_then(|()| writer.flush()).is_ok();
        }
        // (Closed: out of the list now, so its program's panel greys.)
        lock(&closing.streams).retain(|stream| stream.id != id);
        closing.waker.request_repaint();
    });
    if spawned.is_err() {
        log::warn!("api: couldn't start an event stream");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder().http_status_as_error(false).build().into()
    }

    /// A library with no songs, playlists or rulesets of its own.
    fn no_library() -> Library {
        let rulesets = crate::smart::builtin().map(|(id, r)| (id.to_string(), r)).into_iter().collect();
        Library { store: SongStore::open(std::env::temp_dir()), wishes: Default::default(), playlists: Vec::new(), rulesets, group_ignore: String::new(), articles: false, online: false, queue: Vec::new(), soundfonts: Vec::new(), scripts: Vec::new() }
    }

    /// A server on a free port with a reader's and a controller's tokens.
    /// A staging folder of a test's own.
    fn staging() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("synththing-api-staged-{}-{n}", std::process::id()))
    }

    fn server() -> (Server, String, String) {
        let (mut reader, read_token) = Access::new("reader");
        reader.read = true;
        let (mut controller, control_token) = Access::new("controller");
        (controller.read, controller.control) = (false, true);
        let server = Server::start(0, false, vec![reader, controller], egui::Context::default(), staging(), None).unwrap();
        (server, read_token, control_token)
    }

    fn get(server: &Server, path: &str, token: &str) -> (u16, String) {
        let mut response = agent().get(format!("http://{}{path}", server.addr())).header("Authorization", format!("Bearer {token}")).call().unwrap();
        (response.status().as_u16(), response.body_mut().read_to_string().unwrap())
    }

    /// Answer the app's side of calls until `request` is done.
    fn answering<T: Send + 'static>(server: &Server, library: impl Fn() -> Library, request: impl FnOnce() -> T + Send + 'static) -> T {
        let done = std::thread::spawn(request);
        while !done.is_finished() {
            for call in server.calls() {
                match call {
                    Call::Command(Command::Quit, reply) => reply.send(Err("not now".into())).unwrap(),
                    Call::Command(_, reply) => reply.send(Ok(())).unwrap(),
                    Call::NowPlaying(reply) => reply.send(NowPlaying::default()).unwrap(),
                    Call::Library(reply) => reply.send(library()).unwrap(),
                    Call::Play { song: Some(song), reply, .. } if song == "missing" => reply.send(Err((404, "no such song".into()))).unwrap(),
                    Call::Play { reply, .. } => reply.send(Ok(())).unwrap(),
                    Call::Queue(QueueEdit::Remove(at), reply) if at > 0 => reply.send(Err((404, "not that many".into()))).unwrap(),
                    Call::NewPlaylist(name, reply) => reply.send(Ok(PlaylistRef { id: "new".into(), name })).unwrap(),
                    Call::UseSoundfont(id, reply) if id == "missing" => reply.send(Err((404, "no such soundfont".into()))).unwrap(),
                    Call::UseScript(id, reply) if id == "missing" => reply.send(Err((404, "no such script".into()))).unwrap(),
                    Call::Set(_, reply) | Call::Edit(_, reply) | Call::Queue(_, reply) | Call::UseSoundfont(_, reply) | Call::UseScript(_, reply) => {
                        reply.send(Ok(())).unwrap()
                    }
                    Call::Add { .. } => unreachable!("no test adds songs here"),
                    Call::Wishlist(query, reply) => {
                        let wish = Wish { note: query, ..Wish::new(Kind::Album, "Modal Soul") };
                        reply.send(vec![WishInfo { id: "w1".into(), wish, got: "partly", have: 7, total: 12, songs: Vec::new(), missing: Vec::new(), albums: Vec::new() }]).unwrap()
                    }
                    Call::Wish { wish, program, reply, .. } => {
                        let wish = Wish { added_by: Some(program), ..wish };
                        reply.send(Ok((WishInfo { id: "w2".into(), wish, got: "no", have: 0, total: 1, songs: Vec::new(), missing: Vec::new(), albums: Vec::new() }, true))).unwrap()
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        done.join().unwrap()
    }

    #[test]
    fn a_token_is_needed_and_its_permissions_hold() {
        let (server, read_token, control_token) = server();
        assert_eq!(get(&server, "/v1", "wrong").0, 401);
        let addr = server.addr();
        let sent = |header: &str, value: &str| {
            let request = agent().get(format!("http://{addr}/v1")).header("Authorization", format!("Bearer {read_token}")).header(header, value);
            request.call().unwrap().status().as_u16()
        };
        assert_eq!(sent("X-Forwarded-For", "203.0.113.9"), 403);
        assert_eq!(sent("Forwarded", "for=203.0.113.9"), 403);
        assert_eq!(sent("Host", "evil.example"), 403);
        assert_eq!(sent("Host", "localhost:7340"), 200);
        let (status, body) = get(&server, "/v1", &read_token);
        assert_eq!(status, 200);
        assert!(body.contains("\"read\":true") && body.contains("\"control\":false"), "{body}");
        assert_eq!(get(&server, "/v1/commands", &control_token).0, 403);
        let (status, body) = get(&server, "/v1/commands", &read_token);
        assert_eq!(status, 200);
        assert!(body.contains("\"id\":\"play\""), "{body}");

        let url = format!("http://{}/v1/commands/", server.addr());
        let post = move |id: &str, token: &str| {
            let (url, token) = (format!("{url}{id}"), format!("Bearer {token}"));
            move || agent().post(url).header("Authorization", token).send_empty().unwrap().status().as_u16()
        };
        let empty = || Library { store: SongStore::open(std::env::temp_dir()), ..no_library() };
        assert_eq!(answering(&server, empty, post("play", &read_token)), 403);
        assert_eq!(answering(&server, empty, post("play", &control_token)), 204);
        assert_eq!(answering(&server, empty, post("quit", &control_token)), 409);
        assert_eq!(answering(&server, empty, post("fly", &control_token)), 404);

        let url = format!("http://{}/v1/play", server.addr());
        let play = move |query: &str, token: &str| {
            let (url, token) = (format!("{url}{query}"), format!("Bearer {token}"));
            move || agent().post(url).header("Authorization", token).send_empty().unwrap().status().as_u16()
        };
        assert_eq!(answering(&server, empty, play("?song=abc", &read_token)), 403);
        assert_eq!(answering(&server, empty, play("", &control_token)), 400);
        assert_eq!(answering(&server, empty, play("?song=abc", &control_token)), 204);
        assert_eq!(answering(&server, empty, play("?playlist=p1&song=abc", &control_token)), 204);
        assert_eq!(answering(&server, empty, play("?song=missing", &control_token)), 404);

        let url = format!("http://{}/v1/", server.addr());
        let set = move |what: &str, token: &str| {
            let (url, token) = (format!("{url}{what}"), format!("Bearer {token}"));
            move || agent().post(url).header("Authorization", token).send_empty().unwrap().status().as_u16()
        };
        assert_eq!(answering(&server, empty, set("seek?to=12.5", &read_token)), 403);
        assert_eq!(answering(&server, empty, set("seek?to=12.5", &control_token)), 204);
        assert_eq!(answering(&server, empty, set("seek?to=-1", &control_token)), 400);
        assert_eq!(answering(&server, empty, set("volume?to=0.4", &control_token)), 204);
        assert_eq!(answering(&server, empty, set("volume?to=2", &control_token)), 400);
        assert_eq!(answering(&server, empty, set("modes?shuffle=true", &read_token)), 403);
        assert_eq!(answering(&server, empty, set("modes?shuffle=true&loop=all&autoplay=group", &control_token)), 204);
        assert_eq!(answering(&server, empty, set("modes", &control_token)), 400);
        assert_eq!(answering(&server, empty, set("modes?loop=twice", &control_token)), 400);
        assert_eq!(answering(&server, empty, set("modes?shuffle=yes", &control_token)), 400);
        assert_eq!(answering(&server, empty, set("soundfonts/abc/use", &read_token)), 403);
        assert_eq!(answering(&server, empty, set("soundfonts/abc/use", &control_token)), 204);
        assert_eq!(answering(&server, empty, set("soundfonts/missing/use", &control_token)), 404);
        assert_eq!(answering(&server, empty, set("scripts/abc/use", &read_token)), 403);
        assert_eq!(answering(&server, empty, set("scripts/abc/use", &control_token)), 204);
        assert_eq!(answering(&server, empty, set("scripts/missing/use", &control_token)), 404);
        assert_eq!(get(&server, "/v1/visualizer/stream?width=640", &read_token).0, 400);
        assert_eq!(get(&server, "/v1/visualizer/stream?fps=0", &read_token).0, 400);
        assert_eq!(get(&server, "/v1/visualizer/frame.jpg", &control_token).0, 403);
        assert_eq!(get(&server, "/v1/visualizer/frame.png", &control_token).0, 403);
        assert_eq!(get(&server, "/v1/visualizer/stream?format=gif", &read_token).0, 400);
        let (status, page) = get(&server, "/v1/visualizer?width=640&height=360&token=x", &read_token);
        assert_eq!(status, 200);
        assert!(page.contains("/v1/visualizer/stream?width=640&amp;height=360&amp;token=x&amp;format=png\""), "{page}");
        let (_, page) = get(&server, "/v1/visualizer?format=jpeg&token=x", &read_token);
        assert!(page.contains("/v1/visualizer/stream?format=jpeg&amp;token=x\""), "{page}");
        assert_eq!(answering(&server, empty, set("queue/move?from=2&to=0", &read_token)), 403);
        assert_eq!(answering(&server, empty, set("queue/move?from=2&to=0", &control_token)), 204);
        assert_eq!(answering(&server, empty, set("queue/move?from=2", &control_token)), 400);

        let url = format!("http://{}/v1/", server.addr());
        let (mut editor, edit_token) = Access::new("editor");
        editor.edit_library = true;
        let mut apps = server.shared.apps.read().unwrap().clone();
        apps.push(editor);
        server.set_apps(&apps);
        let edit = move |delete: bool, what: &str, token: &str| {
            let (url, token) = (format!("{url}{what}"), format!("Bearer {token}"));
            move || {
                let request = if delete {
                    agent().delete(url).header("Authorization", token).call()
                } else {
                    agent().post(url).header("Authorization", token).send_empty()
                };
                request.unwrap().status().as_u16()
            }
        };
        assert_eq!(answering(&server, empty, edit(false, "songs/abc/star", &control_token)), 403);
        assert_eq!(answering(&server, empty, edit(false, "songs/abc/star", &edit_token)), 204);
        assert_eq!(answering(&server, empty, edit(true, "songs/abc/star", &edit_token)), 204);

        assert_eq!(answering(&server, empty, edit(false, "queue?song=abc", &read_token)), 403);
        assert_eq!(answering(&server, empty, edit(false, "queue", &control_token)), 400);
        assert_eq!(answering(&server, empty, edit(false, "queue?song=abc&playlist=p1", &control_token)), 400);
        assert_eq!(answering(&server, empty, edit(false, "queue?song=abc&next=maybe", &control_token)), 400);
        assert_eq!(answering(&server, empty, edit(false, "queue?playlist=p1&next=true", &control_token)), 204);
        assert_eq!(answering(&server, empty, edit(true, "queue?at=0", &control_token)), 204);
        assert_eq!(answering(&server, empty, edit(true, "queue?at=3", &control_token)), 404);
        assert_eq!(answering(&server, empty, edit(true, "queue?at=first", &control_token)), 400);
        assert_eq!(answering(&server, empty, edit(true, "queue", &control_token)), 204);
        assert_eq!(answering(&server, empty, edit(false, "songs/abc/tags", &edit_token)), 400);
        assert_eq!(answering(&server, empty, edit(false, "songs/abc/tags?tag=chill", &edit_token)), 204);
        assert_eq!(answering(&server, empty, edit(true, "playlists/p1/songs", &edit_token)), 400);
        assert_eq!(answering(&server, empty, edit(true, "playlists/p1/songs?song=abc", &edit_token)), 204);
        assert_eq!(answering(&server, empty, edit(false, "playlists?name=Mornings", &control_token)), 403);
        assert_eq!(answering(&server, empty, edit(false, "playlists?name=Mornings", &edit_token)), 201);
        assert_eq!(answering(&server, empty, edit(false, "playlists?name=%20", &edit_token)), 400);
        assert_eq!(answering(&server, empty, edit(false, "playlists/p1/rename?to=Evenings", &edit_token)), 204);
        assert_eq!(answering(&server, empty, edit(false, "playlists/p1/rename", &edit_token)), 400);
        assert_eq!(answering(&server, empty, edit(false, "playlists/p1/songs/move?from=2&to=0", &edit_token)), 204);
        assert_eq!(answering(&server, empty, edit(false, "playlists/p1/songs/move?to=0", &edit_token)), 400);
        assert_eq!(answering(&server, empty, edit(true, "playlists/p1", &control_token)), 403);
        assert_eq!(answering(&server, empty, edit(true, "playlists/p1", &edit_token)), 204);

        let (status, body) = answering(&server, empty, fetch_as(&server, "/v1/wishlist?q=got:partly", &read_token));
        assert_eq!(status, 200);
        assert!(body.contains(r#""id":"w1","kind":"album","title":"Modal Soul""#) && body.contains(r#""got":"partly","have":7,"total":12"#), "{body}");
        assert!(body.contains(r#""note":"got:partly""#), "the query is the app's to filter by: {body}");
        assert_eq!(answering(&server, empty, edit(true, "wishlist/w1", &control_token)), 403);
        assert_eq!(answering(&server, empty, edit(true, "wishlist/w1", &edit_token)), 204);
        assert_eq!(answering(&server, empty, edit(false, "wishlist/w1/note?text=from%20Willy", &edit_token)), 204);
        let (status, body) = answering(&server, empty, wish_json(&server, r#"{"kind": "song", "title": "Feather", "musicbrainz": "c1"}"#, &edit_token));
        assert_eq!(status, 201);
        assert!(body.contains(r#""added_by":"editor""#), "the program's: {body}");
        assert_eq!(answering(&server, empty, wish_json(&server, r#"{"kind": "song", "title": " "}"#, &edit_token)).0, 400);
        assert_eq!(answering(&server, empty, wish_json(&server, r#"{"kind": "playlist"}"#, &edit_token)).0, 400);
        assert_eq!(answering(&server, empty, wish_json(&server, r#"{"kind": "text", "title": "", "artist": "Toby Fox"}"#, &edit_token)).0, 201);
        assert_eq!(answering(&server, empty, wish_json(&server, r#"{"kind": "text", "title": "x"}"#, &control_token)).0, 403);
        assert_eq!(answering(&server, empty, fetch_as(&server, "/v1/musicbrainz/search", &read_token)).0, 400);
        assert_eq!(answering(&server, empty, fetch_as(&server, "/v1/musicbrainz/search?q=nujabes&kind=label", &read_token)).0, 400);
        assert_eq!(answering(&server, empty, fetch_as(&server, "/v1/musicbrainz/search?q=nujabes", &read_token)).0, 409, "online lookups are off");
        assert_eq!(answering(&server, empty, fetch_as(&server, "/v1/musicbrainz/artists/a1/albums", &read_token)).0, 409);
        assert_eq!(answering(&server, empty, fetch_as(&server, "/v1/musicbrainz/releases/r1", &read_token)).0, 409);
    }

    /// A GET of `path` with `token`, for [`answering`].
    fn fetch_as(server: &Server, path: &str, token: &str) -> impl FnOnce() -> (u16, String) + Send + 'static {
        let (url, token) = (format!("http://{}{path}", server.addr()), format!("Bearer {token}"));
        move || {
            let mut response = agent().get(url).header("Authorization", token).call().unwrap();
            (response.status().as_u16(), response.body_mut().read_to_string().unwrap())
        }
    }

    /// A `POST /v1/wishlist` of `body` with `token`, for [`answering`].
    fn wish_json(server: &Server, body: &str, token: &str) -> impl FnOnce() -> (u16, String) + Send + 'static {
        let (url, body, token) = (format!("http://{}/v1/wishlist", server.addr()), body.to_string(), format!("Bearer {token}"));
        move || {
            let mut response = agent().post(url).header("Authorization", token).send(body).unwrap();
            (response.status().as_u16(), response.body_mut().read_to_string().unwrap())
        }
    }

    #[test]
    fn byte_ranges_are_read_like_http_says() {
        assert_eq!(byte_range("bytes=0-99", 1000), Ok(Some((0, 99))));
        assert_eq!(byte_range("bytes=500-", 1000), Ok(Some((500, 999))));
        assert_eq!(byte_range("bytes=-100", 1000), Ok(Some((900, 999))));
        assert_eq!(byte_range("bytes=-5000", 1000), Ok(Some((0, 999))));
        assert_eq!(byte_range("bytes=900-5000", 1000), Ok(Some((900, 999))));
        assert_eq!(byte_range("bytes=1000-", 1000), Err(()));
        assert_eq!(byte_range("bytes=-0", 1000), Err(()));
        assert_eq!(byte_range("bytes=0-1,5-6", 1000), Ok(None));
        assert_eq!(byte_range("bytes=9-1", 1000), Ok(None));
        assert_eq!(byte_range("items=0-1", 1000), Ok(None));
    }

    #[test]
    fn songs_are_searched_by_the_search_box_rules() {
        let dir = std::env::temp_dir().join(format!("synththing-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = SongStore::open(dir.clone());
        store.add(song_store::write_in(b"one", "Alpha Theme.mid", &dir).unwrap());
        store.add(song_store::write_in(b"two", "Beta Theme.mid", &dir).unwrap());
        let (server, read_token, _) = server();
        let library = move || Library { store: store.clone(), ..no_library() };
        let addr = server.addr();
        let search = move |q: &str| {
            let (url, token) = (format!("http://{addr}/v1/songs?q={q}"), format!("Bearer {read_token}"));
            move || agent().get(url).header("Authorization", token).call().unwrap().body_mut().read_to_string().unwrap()
        };
        let all: serde_json::Value = serde_json::from_str(&answering(&server, &library, search("theme"))).unwrap();
        assert_eq!(all["total"], 2);
        let one: serde_json::Value = serde_json::from_str(&answering(&server, &library, search("beta"))).unwrap();
        assert_eq!(one["total"], 1);
        assert_eq!(one["songs"][0]["file_name"], "Beta Theme.mid");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn songs_since_a_revision_are_only_those_changed() {
        let dir = std::env::temp_dir().join(format!("synththing-api-since-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Mutex::new(SongStore::open(dir.clone())));
        let (alpha, beta) = (song_store::write_in(b"one", "Alpha.mid", &dir).unwrap(), song_store::write_in(b"two", "Beta.mid", &dir).unwrap());
        let (alpha_key, beta_key) = (alpha.key.clone(), beta.key.clone());
        store.lock().unwrap().add(alpha);
        store.lock().unwrap().add(beta);
        let (server, read_token, _) = server();
        let library = {
            let store = Arc::clone(&store);
            move || Library { store: store.lock().unwrap().clone(), ..no_library() }
        };
        let addr = server.addr();
        let ask = |query: String| {
            let (url, token) = (format!("http://{addr}/v1/songs?{query}"), format!("Bearer {read_token}"));
            let body = answering(&server, &library, move || agent().get(url).header("Authorization", token).call().unwrap().body_mut().read_to_string().unwrap());
            serde_json::from_str::<serde_json::Value>(&body).unwrap()
        };
        let all = ask(String::new());
        assert_eq!((all["total"].clone(), all["full"].clone()), (2.into(), true.into()));
        let revision = all["revision"].as_str().unwrap().to_string();
        let none = ask(format!("since={revision}"));
        assert_eq!((none["total"].clone(), none["full"].clone(), none["revision"].clone()), (0.into(), false.into(), revision.clone().into()));
        // (One changed, one gone.)
        store.lock().unwrap().set_added_by(&alpha_key, "downloader");
        let beta_path = store.lock().unwrap().path(&beta_key);
        store.lock().unwrap().remove(&beta_path).unwrap();
        let changed = ask(format!("since={revision}"));
        assert_eq!(changed["total"], 1);
        assert_eq!(changed["songs"][0]["key"], alpha_key.as_str());
        assert_eq!(changed["removed"], serde_json::json!([beta_key]));
        assert_ne!(changed["revision"], revision.as_str());
        // (Changed so the query no longer finds it: it left that list.)
        let left = ask(format!("since={revision}&q=-addedby:downloader"));
        assert_eq!(left["total"], 0);
        assert_eq!(left["removed"], serde_json::json!([alpha_key, beta_key].into_iter().collect::<std::collections::BTreeSet<_>>()));
        // (A revision from another run means everything again.)
        assert_eq!(ask("since=1.1".into())["full"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn songs_a_program_added_are_found_by_it() {
        let dir = std::env::temp_dir().join(format!("synththing-api-added-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = SongStore::open(dir.clone());
        let mine = song_store::write_in(b"one", "Mine.mid", &dir).unwrap();
        let sent = song_store::write_in(b"two", "Sent.mid", &dir).unwrap();
        let sent_key = sent.key.clone();
        store.add(mine);
        store.add(sent);
        store.set_added_by(&sent_key, "downloader");
        let found = matching(&store, &Default::default(), "addedby:downloader");
        assert_eq!(found.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), [sent_key.as_str()]);
        assert_eq!(matching(&store, &Default::default(), "-addedby:downloader").len(), 1);
        assert_eq!(SongInfo::of(&sent_key, found[0].1, &store).added_by.as_deref(), Some("downloader"));
        // (Synced: another device learns who added it.)
        let other = std::env::temp_dir().join(format!("synththing-api-added-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&other);
        std::fs::create_dir_all(&other).unwrap();
        let mut elsewhere = SongStore::open(other.clone());
        elsewhere.learn(store.known().map(|(k, s)| (k.to_string(), s.clone())));
        assert_eq!(elsewhere.get_by_key(&sent_key).unwrap().added_by.as_deref(), Some("downloader"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn what_plays_is_heard_only_with_its_permission() {
        let (reader, read_token) = Access::new("reader");
        let (mut listener, listen_token) = Access::new("listener");
        let (mut both, both_token) = Access::new("both");
        both.listen = true;
        (listener.read, listener.listen) = (false, true);
        let listeners = Listeners::new();
        let server = Server::start(0, false, vec![reader, listener, both], egui::Context::default(), staging(), Some(Arc::clone(&listeners))).unwrap();
        assert_eq!(get(&server, "/v1/audio/stream", &read_token).0, 403);
        assert_eq!(get(&server, "/v1/audio", &read_token).0, 403);
        assert_eq!(get(&server, "/v1/visualizer?audio=1", &read_token).0, 403, "the visualizer with sound needs both");
        assert_eq!(get(&server, "/v1/visualizer?audio=1", &listen_token).0, 403, "the visualizer with sound needs both");
        let (status, page) = get(&server, "/v1/audio?token=x", &listen_token);
        assert_eq!(status, 200);
        assert!(page.contains("/v1/audio/stream?token=x") && !page.contains("<img"), "{page}");
        let (_, page) = get(&server, "/v1/visualizer?width=640&height=360&audio=1&token=x", &both_token);
        assert!(page.contains(r#"const STREAM = "/v1/audio/stream?width=640&height=360&audio=1&token=x";"#), "{page}");

        let url = format!("http://{}/v1/audio/stream", server.addr());
        let response = agent().get(url).header("Authorization", format!("Bearer {listen_token}")).call().unwrap();
        assert_eq!(response.headers().get("Content-Type").unwrap(), "audio/wav");
        let mut body = response.into_body().into_reader();
        let mut header = [0u8; 44];
        body.read_exact(&mut header).unwrap();
        assert_eq!(header, crate::audio_stream::wav_header(SAMPLE_RATE));
        assert!(listeners.listening());
        listeners.send(&[0.5, -0.5]);
        let mut sample = [0u8; 4];
        body.read_exact(&mut sample).unwrap();
        assert_eq!(sample, [0xff, 0x3f, 0x01, 0xc0]);
    }

    #[test]
    fn songs_are_downloaded_only_with_their_permission() {
        let dir = std::env::temp_dir().join(format!("synththing-api-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = SongStore::open(dir.clone());
        let stored = song_store::write_in(b"MThd tune", "Überture.mid", &dir).unwrap();
        let key = stored.key.clone();
        store.add(stored);
        let (reader, read_token) = Access::new("reader");
        let (mut fetcher, fetch_token) = Access::new("fetcher");
        fetcher.download_songs = true;
        let server = Server::start(0, false, vec![reader, fetcher], egui::Context::default(), staging(), None).unwrap();
        let library = move || Library { store: store.clone(), ..no_library() };
        let addr = server.addr();
        let fetch = move |key: &str, token: &str| {
            let (url, token) = (format!("http://{addr}/v1/songs/{key}/file"), format!("Bearer {token}"));
            move || {
                let mut response = agent().get(url).header("Authorization", token).call().unwrap();
                let disposition = response.headers().get("Content-Disposition").map(|v| v.to_str().unwrap().to_string());
                (response.status().as_u16(), disposition, response.body_mut().read_to_vec().unwrap())
            }
        };
        assert_eq!(answering(&server, &library, fetch(&key, &read_token)).0, 403);
        let (status, disposition, body) = answering(&server, &library, fetch(&key, &fetch_token));
        assert_eq!((status, body.as_slice()), (200, &b"MThd tune"[..]));
        assert_eq!(disposition.as_deref(), Some("attachment; filename=\"_berture.mid\"; filename*=UTF-8''%C3%9Cberture.mid"));
        assert_eq!(answering(&server, &library, fetch("nope", &fetch_token)).0, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adding_songs_needs_its_own_permission() {
        let (mut reader, read_token) = Access::new("reader");
        reader.control = true;
        let (mut adder, add_token) = Access::new("adder");
        adder.add_songs = true;
        let server = Server::start(0, false, vec![reader, adder], egui::Context::default(), staging(), None).unwrap();
        let post = |query: &str, token: &str| {
            agent()
                .post(format!("http://{}/v1/songs{query}", server.addr()))
                .header("Authorization", format!("Bearer {token}"))
                .send(&b"MThd"[..])
                .unwrap()
                .status()
                .as_u16()
        };
        assert_eq!(post("?name=Song.mid", &read_token), 403);
        assert_eq!(post("", &add_token), 400);
        assert_eq!(post("?name=notes.txt", &add_token), 415);
        assert_eq!(post("?name=..%2Fescape.mid", &add_token), 400);
    }

    #[test]
    fn songs_are_staged_and_dropped() {
        let (mut adder, add_token) = Access::new("adder");
        adder.add_songs = true;
        let (reader, read_token) = Access::new("reader");
        let server = Server::start(0, false, vec![reader, adder], egui::Context::default(), staging(), None).unwrap();
        let addr = server.addr();
        let send = move |method: &'static str, path: &str, token: &str| {
            let (url, token) = (format!("http://{addr}{path}"), format!("Bearer {token}"));
            move || {
                let midi = b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk\0\0\0\x04\0\xff\x2f\0".to_vec();
                let mut response = match method {
                    "POST" => agent().post(url).header("Authorization", token).send(&midi[..]).unwrap(),
                    "DELETE" => agent().delete(url).header("Authorization", token).call().unwrap(),
                    _ => agent().get(url).header("Authorization", token).call().unwrap(),
                };
                (response.status().as_u16(), response.body_mut().read_to_string().unwrap())
            }
        };
        assert_eq!(answering(&server, no_library, send("POST", "/v1/staged?name=Theme.mid", &read_token)).0, 403);
        let (status, body) = answering(&server, no_library, send("POST", "/v1/staged?name=Theme.mid", &add_token));
        assert_eq!(status, 201, "{body}");
        let staged: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!((staged["file_name"].clone(), staged["here"].clone()), ("Theme.mid".into(), false.into()));
        let key = staged["key"].as_str().unwrap().to_string();
        assert_eq!(answering(&server, no_library, send("POST", "/v1/staged?name=Theme.mid", &add_token)).0, 200, "staged already");
        let (status, body) = answering(&server, no_library, send("GET", "/v1/staged", &add_token));
        assert_eq!(status, 200);
        assert!(body.contains(&key), "{body}");
        assert_eq!(answering(&server, no_library, send("POST", "/v1/staged/check", &add_token)).0, 409, "online lookups are off");
        assert_eq!(answering(&server, no_library, send("POST", "/v1/staged/nope/import", &add_token)).0, 404);
        assert_eq!(answering(&server, no_library, send("DELETE", &format!("/v1/staged/{key}"), &add_token)).0, 204);
        assert_eq!(answering(&server, no_library, send("DELETE", &format!("/v1/staged/{key}"), &add_token)).0, 404);
        assert_eq!(answering(&server, no_library, send("GET", "/v1/staged", &add_token)), (200, "[]".to_string()));
    }

    #[test]
    fn groupings_are_listed_and_split() {
        let (server, read_token, _) = server();
        let addr = server.addr();
        let fetch = move |path: &str| {
            let (url, token) = (format!("http://{addr}{path}"), format!("Bearer {read_token}"));
            move || {
                let mut response = agent().get(url).header("Authorization", token).call().unwrap();
                (response.status().as_u16(), response.body_mut().read_to_string().unwrap())
            }
        };
        let (status, body) = answering(&server, no_library, fetch("/v1/groupings"));
        assert_eq!(status, 200);
        assert!(body.contains("\"id\":\"albums\"") && body.contains("\"split\":\"album_artist\""), "{body}");
        let (status, body) = answering(&server, no_library, fetch("/v1/groupings/albums"));
        assert_eq!(status, 200);
        assert!(body.contains("\"groups\":[]"), "{body}");
        assert_eq!(answering(&server, no_library, fetch("/v1/groupings/nope")).0, 404);
        assert_eq!(answering(&server, no_library, fetch("/v1/groups")).0, 400);
        assert_eq!(answering(&server, no_library, fetch("/v1/groups?split=mood")).0, 400);
        assert_eq!(answering(&server, no_library, fetch("/v1/groups?split=genre&q=x")), (200, "[]".to_string()));
        assert_eq!(answering(&server, no_library, fetch("/v1/songs/nope/art")).0, 404);
    }

    #[test]
    fn events_reach_open_streams() {
        let (server, read_token, _) = server();
        let url = format!("http://{}/v1/events?token={read_token}", server.addr());
        let stream = std::thread::spawn(move || {
            let response = agent().get(url).call().unwrap();
            let mut lines = BufReader::new(response.into_body().into_reader()).lines();
            lines.find(|l| l.as_ref().is_ok_and(|l| l.starts_with("event:"))).unwrap().unwrap()
        });
        while !server.listened_to() {
            std::thread::sleep(Duration::from_millis(5));
        }
        server.event("now_playing", &NowPlaying::default());
        assert_eq!(stream.join().unwrap(), "event: now_playing");
    }

    #[test]
    fn a_stream_gets_only_the_events_it_names() {
        let (server, read_token, _) = server();
        let addr = server.addr();
        let bogus = agent().get(format!("http://{addr}/v1/events?token={read_token}&events=nope")).call().unwrap();
        assert_eq!(bogus.status().as_u16(), 400);
        let url = format!("http://{addr}/v1/events?token={read_token}&events=seeked");
        let stream = std::thread::spawn(move || {
            let response = agent().get(url).call().unwrap();
            let mut lines = BufReader::new(response.into_body().into_reader()).lines();
            lines.find(|l| l.as_ref().is_ok_and(|l| l.starts_with("event:"))).unwrap().unwrap()
        });
        while !server.listened_to() {
            std::thread::sleep(Duration::from_millis(5));
        }
        server.event("now_playing", &NowPlaying::default());
        server.event("seeked", &serde_json::json!({ "position": 1.0 }));
        assert_eq!(stream.join().unwrap(), "event: seeked");
    }

    #[test]
    fn shared_data_is_read_with_read_and_written_with_control() {
        let (server, read_token, control_token) = server();
        let addr = server.addr();
        let put = |token: &str, body: &str| {
            let request = agent().put(format!("http://{addr}/v1/shared/api%20test")).header("Authorization", format!("Bearer {token}"));
            request.send(body).unwrap().status().as_u16()
        };
        assert_eq!(put(&read_token, "1"), 403);
        assert_eq!(put(&control_token, "not json"), 400);
        assert_eq!(put(&control_token, r#"{"score": 3}"#), 204);
        assert_eq!(get(&server, "/v1/shared/api%20test", &control_token).0, 403);
        assert_eq!(get(&server, "/v1/shared/api%20test", &read_token), (200, r#"{"score":3}"#.to_string()));
        assert!(get(&server, "/v1/shared", &read_token).1.contains(r#""api test":{"score":3}"#));
        let delete = || {
            let request = agent().delete(format!("http://{addr}/v1/shared/api%20test")).header("Authorization", format!("Bearer {control_token}"));
            request.call().unwrap().status().as_u16()
        };
        assert_eq!(delete(), 204);
        assert_eq!(delete(), 404);
        assert_eq!(get(&server, "/v1/shared/api%20test", &read_token).0, 404);
    }

    #[test]
    fn a_panel_is_shown_and_its_events_reach_only_its_program() {
        let (reader, read_token) = Access::new("reader");
        let (mut finder, finder_token) = Access::new("Finder");
        finder.panel = true;
        let finder_hash = finder.token.clone();
        let server = Server::start(0, false, vec![reader, finder], egui::Context::default(), staging(), None).unwrap();
        let addr = server.addr();
        let put = |token: &str, body: &str| {
            let request = agent().put(format!("http://{addr}/v1/panel")).header("Authorization", format!("Bearer {token}"));
            request.send(body).unwrap().status().as_u16()
        };
        let panel = r#"{"title": "Finder", "elements": [{"id": "q", "type": "textbox"}]}"#;
        assert_eq!(put(&read_token, panel), 403);
        assert_eq!(put(&finder_token, r#"{"title": "Finder"}"#), 400);
        assert_eq!(put(&finder_token, &"x".repeat(crate::panel::LARGEST + 1)), 413);
        assert_eq!(put(&finder_token, panel), 204);
        let mut revision = 0;
        let shown = server.panels(&mut revision).unwrap();
        assert_eq!((shown.len(), shown[0].name.as_str(), shown[0].app.as_str()), (1, "Finder", finder_hash.as_str()));
        assert!(server.panels(&mut revision).is_none(), "nothing new since");
        let patch = |id: &str, body: &str| {
            let request = agent().patch(format!("http://{addr}/v1/panel/elements/{id}")).header("Authorization", format!("Bearer {finder_token}"));
            request.send(body).unwrap().status().as_u16()
        };
        assert_eq!(patch("q", r#"{"value": "overworld"}"#), 204);
        assert_eq!(patch("nope", r#"{"hint": "x"}"#), 404);
        assert_eq!(patch("q", r#"{"multiline": "yes"}"#), 400);
        let shown = server.panels(&mut revision).unwrap();
        let given = &shown[0].given;
        assert_eq!((given.len(), given[0].id.as_str(), given[0].value.as_str(), given[0].revision), (1, "q", "overworld", revision));
        assert!(matches!(&shown[0].panel.elements[0].kind, crate::panel::Kind::Textbox { value: Some(v), .. } if v == "overworld"));
        assert_eq!(patch("q", r#"{"hint": "Search"}"#), 204);
        assert_eq!(server.panels(&mut revision).unwrap()[0].given[0].revision, revision - 1, "a value given once stays given once");
        assert!(!server.connected(&finder_hash));

        // (The reader's stream hears nothing of Finder's panel; Finder's own
        // stream does.)
        let listen = |token: String| {
            let url = format!("http://{addr}/v1/events?token={token}");
            std::thread::spawn(move || {
                let response = agent().get(url).call().unwrap();
                let mut lines = BufReader::new(response.into_body().into_reader()).lines();
                lines.find(|l| l.as_ref().is_ok_and(|l| l.starts_with("event:"))).unwrap().unwrap()
            })
        };
        let reader_stream = listen(read_token);
        let finder_stream = listen(finder_token.clone());
        while lock(&server.shared.streams).len() < 2 {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(server.connected(&finder_hash));
        server.panel_event(&finder_hash, &crate::panel::Event::of("go", crate::panel::EventKind::Clicked));
        assert_eq!(finder_stream.join().unwrap(), "event: panel");
        server.event("seeked", &serde_json::json!({ "position": 1.0 }));
        assert_eq!(reader_stream.join().unwrap(), "event: seeked");

        // (The first stream goes at its next ping, its reader gone: wait,
        // so the one opened next is the one the goodbye has to close.)
        while server.connected(&finder_hash) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let finder_stream = listen(finder_token.clone());
        while !server.connected(&finder_hash) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let goodbye = agent().delete(format!("http://{addr}/v1/events")).header("Authorization", format!("Bearer {finder_token}")).call().unwrap();
        assert_eq!(goodbye.status().as_u16(), 204);
        assert!(!server.connected(&finder_hash), "a goodbye closes its streams at once");
        drop(finder_stream);

        let delete = agent().delete(format!("http://{addr}/v1/panel")).header("Authorization", format!("Bearer {finder_token}")).call().unwrap();
        assert_eq!(delete.status().as_u16(), 204);
        assert_eq!(server.panels(&mut revision), Some(Vec::new()));
    }
}
