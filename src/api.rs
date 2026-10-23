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

use crate::commands::Command;
use crate::fields::{Facts, Field};
use crate::smart::{Group, Order, Rules, Ruleset, Split, SplitBy};
use crate::song_store::{self, Song, SongStore, Stored};

/// The port it listens on unless it's given another.
pub const DEFAULT_PORT: u16 = 7340;
/// Request handlers (a stream gets a thread of its own besides).
const WORKERS: usize = 2;
/// Streams open at once.
const MOST_STREAMS: usize = 32;
/// How long a stream goes quiet before a keep-alive comment.
const KEEP_ALIVE: Duration = Duration::from_secs(25);
/// How long a request waits for the app to answer.
const APP_WAIT: Duration = Duration::from_secs(5);
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
}

impl Access {
    /// A new app's access and the token to give it (shown once: only its
    /// hash is kept).
    pub fn new(name: &str) -> (Access, String) {
        let mut bytes = [0u8; 24];
        getrandom::fill(&mut bytes).expect("the OS's random numbers");
        let token = format!("st_{}", crate::library::hex(&bytes));
        let access = Access { name: name.to_string(), token: crate::library::sha256_hex(token.as_bytes()), read: true, control: false, add_songs: false };
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
    /// A song a program sent (`program`, by its name), already in the
    /// store's folder, to add to the library: the song, and whether it's
    /// new to the library (only then is it marked as the program's).
    Add {
        stored: Stored,
        program: String,
        reply: Sender<Result<(SongInfo, bool), String>>,
    },
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
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlaylistRef {
    pub id: String,
    pub name: String,
}

/// The library as the app has it, to search away from the app's thread.
pub struct Library {
    pub store: SongStore,
    pub playlists: Vec<PlaylistInfo>,
    /// Every ruleset, by ID: the built-in ones, then the user's.
    pub rulesets: Vec<(String, Ruleset)>,
    /// How groups are told apart and sorted (`Config::group_ignore`,
    /// `Config::sort_with_articles`).
    pub group_ignore: String,
    pub articles: bool,
    /// Whether covers not kept yet may be downloaded.
    pub online: bool,
}

impl Library {
    /// The playlists `rules` make of the songs on this device (as
    /// `/v1/songs` has them: a song known only from another device is
    /// left out, and a playlist of none of these with it).
    fn groups(&self, rules: &Rules) -> Vec<GroupInfo> {
        let groups = rules.groups(&self.store, crate::settings_sync::now(), &self.group_ignore, self.articles);
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
    streams: Mutex<Vec<Sender<String>>>,
}

impl Server {
    /// Listen on `port`, on this computer only unless `lan`.
    pub fn start(port: u16, lan: bool, apps: Vec<Access>, waker: egui::Context) -> Result<Server, String> {
        let http = tiny_http::Server::http((if lan { "0.0.0.0" } else { "127.0.0.1" }, port)).map_err(|e| format!("couldn't listen on port {port}: {e}"))?;
        let addr = http.server_addr().to_ip().ok_or("not listening on an IP address")?;
        let http = Arc::new(http);
        let (tx, calls) = mpsc::channel();
        let shared = Arc::new(Shared { lan, apps: RwLock::new(apps), calls: Mutex::new(tx), waker, streams: Mutex::default() });
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

    /// The apps allowed in, as the preferences now have them.
    pub fn set_apps(&self, apps: &[Access]) {
        if *self.shared.apps.read().unwrap_or_else(|p| p.into_inner()) != apps {
            *self.shared.apps.write().unwrap_or_else(|p| p.into_inner()) = apps.to_vec();
        }
    }

    /// The calls waiting for the app.
    pub fn calls(&self) -> impl Iterator<Item = Call> + '_ {
        self.calls.try_iter()
    }

    /// Whether any event stream is open (events are only worked out then).
    pub fn listened_to(&self) -> bool {
        !self.shared.streams.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    }

    /// Send an event to every open stream.
    pub fn event(&self, name: &str, data: &impl Serialize) {
        let Ok(data) = serde_json::to_string(data) else { return };
        let message = format!("event: {name}\ndata: {data}\n\n");
        self.shared.streams.lock().unwrap_or_else(|p| p.into_inner()).retain(|stream| stream.send(message.clone()).is_ok());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // (Each unblock frees one worker; dropping the senders ends the
        // streams.)
        for _ in &self.threads {
            self.http.unblock();
        }
        self.shared.streams.lock().unwrap_or_else(|p| p.into_inner()).clear();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

type Reply = Response<Cursor<Vec<u8>>>;

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
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let query = crate::library::server::parse_query(query);
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
            }),
        )),
        (Method::Get, ["v1", "events"]) => match needs(access.read, "read") {
            Some(reply) => Some(reply),
            None => {
                stream(shared, request);
                return;
            }
        },
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
            match ask(shared, |reply| Call::Play { song, playlist, reply }) {
                Ok(Ok(())) => Response::from_data(Vec::new()).with_status_code(204),
                Ok(Err((status, why))) => error(status, why),
                Err(reply) => reply,
            }
        })),
        (Method::Get, ["v1", "now-playing"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::NowPlaying).map_or_else(|reply| reply, |now| json(200, &now))))
        }
        (Method::Get, ["v1", "songs"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::Library).map_or_else(|reply| reply, |library| songs(&library.store, &query))))
        }
        (Method::Post, ["v1", "songs"]) => Some(match needs(access.add_songs, "add songs") {
            Some(reply) => reply,
            None => add_song(shared, &mut request, &access, &query),
        }),
        (Method::Get, ["v1", "songs", key]) => Some(needs(access.read, "read").unwrap_or_else(|| {
            ask(shared, Call::Library).map_or_else(
                |reply| reply,
                |library| match library.store.get_by_key(key) {
                    Some(song) => json(200, &SongInfo::of(key, song, &library.store)),
                    None => error(404, "there's no song with that key"),
                },
            )
        })),
        (Method::Get, ["v1", "songs", key, "art"]) => {
            Some(needs(access.read, "read").unwrap_or_else(|| ask(shared, Call::Library).map_or_else(|reply| reply, |library| art(&library, key))))
        }
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
    let Some(name) = query.get("name").map(|n| n.trim()).filter(|n| !n.is_empty() && !n.contains(['/', '\\'])) else {
        return error(400, "say the song's file name: ?name=Song.mp3");
    };
    if !song_store::takes(std::path::Path::new(name), crate::app::SONG_EXTENSIONS) {
        return error(415, format!("{name} isn't a file synththing can play"));
    }
    if request.body_length().is_some_and(|n| n > LARGEST_SONG) {
        return error(413, "that song is too big (250 MB at most)");
    }
    let mut bytes = Vec::new();
    match request.as_reader().take(LARGEST_SONG as u64 + 1).read_to_end(&mut bytes) {
        Ok(_) if bytes.len() > LARGEST_SONG => return error(413, "that song is too big (250 MB at most)"),
        Ok(_) if bytes.is_empty() => return error(400, "send the song's file as the body"),
        Ok(_) => {}
        Err(e) => return error(400, format!("couldn't read the song: {e}")),
    }
    let stored = match crate::config::music_dir().and_then(|dir| song_store::write_in(&bytes, name, &dir)) {
        Ok(stored) => stored,
        Err(e) => return error(500, format!("couldn't store the song: {e:#}")),
    };
    match ask(shared, |reply| Call::Add { stored, program: access.name.clone(), reply }) {
        Ok(Ok((song, new))) => json(if new { 201 } else { 200 }, &song),
        Ok(Err(why)) => error(503, why),
        Err(reply) => reply,
    }
}

#[derive(Serialize)]
struct SongPage {
    /// Songs the query matches, in all.
    total: usize,
    songs: Vec<SongInfo>,
}

/// `GET /v1/songs?q=&offset=&limit=`: the songs the query matches (the
/// Songs search box's filters and text), in file name order.
fn songs(store: &SongStore, query: &HashMap<String, String>) -> Reply {
    let q = query.get("q").map_or("", String::as_str);
    let offset = query.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
    let limit = query.get("limit").and_then(|l| l.parse().ok()).unwrap_or(MOST_SONGS).min(MOST_SONGS);
    let found = matching(store, q);
    let total = found.len();
    let songs = found.into_iter().skip(offset).take(limit).map(|(key, song)| SongInfo::of(&key, song, store)).collect();
    json(200, &SongPage { total, songs })
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
    let found: std::collections::HashSet<String> = matching(&library.store, q).into_iter().map(|(key, _)| key).collect();
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

/// `GET /v1/songs/{key}/art`: the song's picture (its own, else its
/// release's cover), as the app would show it for its album.
fn art(library: &Library, key: &str) -> Reply {
    let Some(song) = library.store.get_by_key(key) else { return error(404, "there's no song with that key") };
    let source = crate::album_art::Source {
        file: library.store.has(key).then(|| library.store.path(key)),
        release: crate::lookup::release_of(song).map(|(release, group)| (release.to_string(), group.map(str::to_string))),
        chosen: song.online.as_ref().and_then(|o| o.art.clone()),
    };
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
fn matching<'s>(store: &'s SongStore, query: &str) -> Vec<(String, &'s Song)> {
    let search = crate::search::parse(query, store.tags());
    let rules = search.rules.matcher(crate::settings_sync::now());
    let mut found: Vec<(String, &Song)> = store
        .songs()
        .filter(|(_, song)| rules(song))
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
fn stream(shared: &Arc<Shared>, request: Request) {
    let (tx, rx) = mpsc::channel::<String>();
    {
        let mut streams = shared.streams.lock().unwrap_or_else(|p| p.into_inner());
        if streams.len() >= MOST_STREAMS {
            drop(streams);
            let _ = request.respond(error(503, "too many open event streams"));
            return;
        }
        streams.push(tx);
    }
    let spawned = std::thread::Builder::new().name("api events".into()).spawn(move || {
        let mut writer = request.into_writer();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
                    Connection: close\r\n\r\n: open\n\n";
        let mut ok = writer.write_all(head.as_bytes()).and_then(|()| writer.flush()).is_ok();
        while ok {
            let chunk = match rx.recv_timeout(KEEP_ALIVE) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => ": keep-alive\n\n".to_string(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            ok = writer.write_all(chunk.as_bytes()).and_then(|()| writer.flush()).is_ok();
        }
        // (Gone: its sender goes with the next event.)
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
        Library { store: SongStore::open(std::env::temp_dir()), playlists: Vec::new(), rulesets, group_ignore: String::new(), articles: false, online: false }
    }

    /// A server on a free port with a reader's and a controller's tokens.
    fn server() -> (Server, String, String) {
        let (mut reader, read_token) = Access::new("reader");
        reader.read = true;
        let (mut controller, control_token) = Access::new("controller");
        (controller.read, controller.control) = (false, true);
        let server = Server::start(0, false, vec![reader, controller], egui::Context::default()).unwrap();
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
                    Call::Add { .. } => unreachable!("no test adds songs here"),
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
        let found = matching(&store, "addedby:downloader");
        assert_eq!(found.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), [sent_key.as_str()]);
        assert_eq!(matching(&store, "-addedby:downloader").len(), 1);
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
    fn adding_songs_needs_its_own_permission() {
        let (mut reader, read_token) = Access::new("reader");
        reader.control = true;
        let (mut adder, add_token) = Access::new("adder");
        adder.add_songs = true;
        let server = Server::start(0, false, vec![reader, adder], egui::Context::default()).unwrap();
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
}
