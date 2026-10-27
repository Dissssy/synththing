//! The hub's own server (`/hub/v1`, issue #192): the user's other devices
//! send it songs and take songs from it directly, instead of through the
//! relay (`song_sync.rs` says when). Plain HTTP is enough: every song goes
//! sealed with the sync key, as on the relay, and every request but
//! `hello` is signed with the identity key, for this hub's own key (its
//! `hub.key`), so it can't be played to another.
//!
//! - `GET /hub/v1/hello?challenge=`: unsigned. Its key and version, and
//!   the challenge signed with its key, so a device knows it reached this
//!   hub, not whatever answers at that address now.
//! - `PUT /hub/v1/songs/{key}`: a song, sealed. Kept only if it's the song
//!   (its hash is its key). 200 if the hub has it already, 409 while
//!   another device sends it (so devices split what they send between
//!   them), 507 when its disk is nearly full.
//! - `POST /hub/v1/songs/{key}/get`: the song, sealed (404: not here).
//! - `POST /hub/v1/live`: a live message (`device_messages.rs`), sealed,
//!   for the user's devices: passed to those listening here, and to the
//!   hub itself (`Came::Live`), which passes it on to the server for those
//!   listening there (`hub.rs`).
//! - `POST /hub/v1/live/listen`: the live messages as they come, as the
//!   server's notification stream has them (`: open`, then
//!   `data: {"live": ...}`), for `client::STREAM_FOR`. A device listening
//!   here takes live messages from here only: the hub passes on those that
//!   come to the server (`HubServer::pass_on`), once each.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use tiny_http::{Header, Method, Request, Response};

use crate::library::identity::{self, Identity};
use crate::library::{self, sync};
use crate::song_store::{self, Incoming};

/// Requests handled at once.
const WORKERS: usize = 8;
/// Songs aren't taken in below this much free space (bytes).
const FREE_FLOOR: u64 = 2 * 1024 * 1024 * 1024;
const CHALLENGE_MAX: usize = 128;
/// Devices listening for live messages at once, at most.
const MOST_LISTENING: usize = 32;
/// Live messages remembered, not to pass one on twice.
const SEEN: usize = 512;
const KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(25);

/// What came to the hub.
#[derive(Debug, PartialEq)]
pub enum Came {
    /// A song, by key, now in the library's folder.
    Song(String),
    /// A live message, sealed, from a device listening here.
    Live(String),
}

/// The hub's server, running until it's dropped.
pub struct HubServer {
    addr: SocketAddr,
    http: Arc<tiny_http::Server>,
    shared: Arc<Shared>,
}

impl Drop for HubServer {
    fn drop(&mut self) {
        self.http.unblock();
    }
}

struct Shared {
    /// The hub's own key.
    key: Identity,
    /// The identity's public key: the only one let in.
    identity: String,
    sync_key: [u8; 32],
    /// The song library's folder.
    dir: PathBuf,
    /// Songs being sent here now.
    incoming: Mutex<BTreeSet<String>>,
    /// Nonces seen, and when, within the time a signed request is good for.
    nonces: Mutex<HashMap<String, i64>>,
    /// What came.
    came: Mutex<Sender<Came>>,
    /// The devices listening for live messages.
    listening: Mutex<Vec<(u64, Sender<String>)>>,
    /// The live messages passed on lately (by their SHA-256), oldest first.
    seen: Mutex<(HashSet<String>, VecDeque<String>)>,
}

impl HubServer {
    /// Listen on `port` (every network), as the hub (its own key `key`) for
    /// `identity`, keeping songs in `dir`: the server, and what comes to it.
    pub fn start(port: u16, key: Identity, identity: &Identity, sync_key: [u8; 32], dir: PathBuf) -> Result<(HubServer, Receiver<Came>), String> {
        let http = tiny_http::Server::http(("0.0.0.0", port)).map_err(|e| format!("couldn't listen on port {port}: {e}"))?;
        let addr = http.server_addr().to_ip().ok_or("not listening on an IP address")?;
        let http = Arc::new(http);
        let (tx, came) = mpsc::channel();
        let shared = Arc::new(Shared {
            key,
            identity: identity.public(),
            sync_key,
            dir,
            incoming: Mutex::default(),
            nonces: Mutex::default(),
            came: Mutex::new(tx),
            listening: Mutex::default(),
            seen: Mutex::default(),
        });
        for _ in 0..WORKERS {
            let (http, shared) = (Arc::clone(&http), Arc::clone(&shared));
            std::thread::Builder::new()
                .name("hub server".into())
                .spawn(move || {
                    while let Ok(request) = http.recv() {
                        handle(&shared, request);
                    }
                    // (Stopped: the streams end too.)
                    lock(&shared.listening).clear();
                })
                .map_err(|e| format!("couldn't start: {e}"))?;
        }
        Ok((HubServer { addr, http, shared }, came))
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Pass a sealed live message on to the devices listening here, unless
    /// it was already: whether it was new.
    pub fn pass_on(&self, sealed: &str) -> bool {
        pass_on(&self.shared, sealed)
    }
}

/// The hub's own key (`hub.key` in the config folder), made the first time.
pub fn hub_key() -> Result<Identity, String> {
    let path = crate::config::config_dir().map_err(|e| format!("{e:#}"))?.join("hub.key");
    if let Some(key) = std::fs::read_to_string(&path).ok().and_then(|text| Identity::from_secret_hex(&text)) {
        return Ok(key);
    }
    let key = Identity::generate()?;
    identity::save_secret(&path, &key.secret_hex())?;
    Ok(key)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn json(status: u16, value: serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(value.to_string()).with_status_code(status).with_header(Header::from_bytes("Content-Type", "application/json").expect("a valid header"))
}

fn error(status: u16, text: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json(status, serde_json::json!({ "error": text }))
}

fn handle(shared: &Arc<Shared>, mut request: Request) {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = path.to_string();
    let method = request.method().clone();
    let segments: Vec<&str> = path.strip_prefix("/hub/v1/").unwrap_or_default().split('/').collect();
    let reply = match (&method, segments.as_slice()) {
        (Method::Get, ["hello"]) => hello(shared, query),
        (Method::Put, ["songs", key]) => match signed(shared, &request, "PUT", &path) {
            Ok(()) => take(shared, &mut request, key),
            Err(reply) => reply,
        },
        (Method::Post, ["songs", key, "get"]) => match signed(shared, &request, "POST", &path) {
            Ok(()) => return give(shared, request, key),
            Err(reply) => reply,
        },
        (Method::Post, ["live"]) => match signed(shared, &request, "POST", &path) {
            Ok(()) => live(shared, &mut request),
            Err(reply) => reply,
        },
        (Method::Post, ["live", "listen"]) => match signed(shared, &request, "POST", &path) {
            Ok(()) => return listen(shared, request),
            Err(reply) => reply,
        },
        _ => error(404, "not found"),
    };
    let _ = request.respond(reply);
}

fn hello(shared: &Shared, query: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let challenge = query.split('&').find_map(|pair| pair.strip_prefix("challenge=")).unwrap_or_default();
    if challenge.is_empty() || challenge.len() > CHALLENGE_MAX || !challenge.bytes().all(|b| b.is_ascii_hexdigit()) {
        return error(400, "a hello needs a challenge (hex)");
    }
    json(
        200,
        serde_json::json!({
            "key": shared.key.public(),
            "version": crate::build_info::VERSION,
            "signature": shared.key.sign(&library::hub_hello_message(challenge)),
        }),
    )
}

/// Whether `request` is signed by the identity, for this hub, now, and not
/// seen before.
fn signed(shared: &Shared, request: &Request, method: &str, path: &str) -> Result<(), Response<std::io::Cursor<Vec<u8>>>> {
    let get = |name: &'static str| request.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().trim().to_string());
    let (Some(key), Some(time), Some(nonce), Some(signature)) =
        (get(library::KEY_HEADER), get(library::TIME_HEADER), get(library::NONCE_HEADER), get(library::SIGNATURE_HEADER))
    else {
        return Err(error(401, "this needs a signed request"));
    };
    if key != shared.identity {
        return Err(error(403, "this hub is someone else's"));
    }
    let time: i64 = time.parse().map_err(|_| error(400, "a signed request's time isn't a number"))?;
    let now = now();
    if (now - time).abs() > library::REQUEST_WINDOW_SECONDS {
        return Err(error(401, "the request's time is too far from the hub's: is the computer's clock right?"));
    }
    if !library::verify_hex(&key, &library::request_message(&shared.key.public(), method, path, time, &nonce, &[]), &signature) {
        return Err(error(401, "the request's signature doesn't match its key"));
    }
    let mut nonces = lock(&shared.nonces);
    nonces.retain(|_, at| (now - *at).abs() <= library::REQUEST_WINDOW_SECONDS);
    if nonces.insert(nonce, now).is_some() {
        return Err(error(401, "that request was already made"));
    }
    Ok(())
}

/// The device a request says it's from, for the log.
fn from(request: &Request) -> String {
    request.headers().iter().find(|h| h.field.equiv(library::DEVICE_HEADER)).map_or_else(|| "a device".into(), |h| h.value.as_str().to_string())
}

/// Takes a song off the list of those coming once it's in (or failed).
struct Coming<'a>(&'a Shared, String);

impl Drop for Coming<'_> {
    fn drop(&mut self) {
        lock(&self.0.incoming).remove(&self.1);
    }
}

fn take(shared: &Shared, request: &mut Request, key: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    if !song_store::is_key(key) {
        return error(400, "that's not a song's key");
    }
    if song_store::path_of(&shared.dir, key).exists() {
        return json(200, serde_json::json!({}));
    }
    if !lock(&shared.incoming).insert(key.to_string()) {
        return error(409, "another device is sending it");
    }
    let _coming = Coming(shared, key.to_string());
    if fs4::available_space(&shared.dir).is_ok_and(|free| free < FREE_FLOOR) {
        log::warn!("not taking a song: under 2 GB left on the disk at {}", shared.dir.display());
        return error(507, "the hub's disk is nearly full");
    }
    let mut incoming = match Incoming::new(key, &shared.dir) {
        Ok(incoming) => incoming,
        Err(e) => return error(500, &format!("{e:#}")),
    };
    match sync::open_stream(&shared.sync_key, request.as_reader(), &mut incoming) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return error(400, "the song didn't open with this hub's sync key"),
        Err(e) => return error(400, &format!("the song didn't arrive ({e})")),
    }
    if let Err(e) = incoming.finish() {
        return error(400, &format!("{e:#}"));
    }
    log::info!("{} sent {key}", from(request));
    let _ = lock(&shared.came).send(Came::Song(key.to_string()));
    json(201, serde_json::json!({}))
}

fn give(shared: &Shared, request: Request, key: &str) {
    let file = song_store::is_key(key).then(|| song_store::path_of(&shared.dir, key)).and_then(|path| std::fs::File::open(path).ok());
    let Some(file) = file else {
        let _ = request.respond(error(404, "the hub doesn't have it"));
        return;
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let sealed = match sync::SealedReader::new(&shared.sync_key, file) {
        Ok(sealed) => sealed,
        Err(e) => {
            let _ = request.respond(error(500, &e));
            return;
        }
    };
    let size = sync::sealed_len(len) as usize;
    let header = Header::from_bytes("Content-Type", "application/octet-stream").expect("a valid header");
    let _ = request.respond(Response::new(tiny_http::StatusCode(200), vec![header], sealed, Some(size), None));
}

/// Pass `sealed` on to the devices listening, unless it was already:
/// whether it was new.
fn pass_on(shared: &Shared, sealed: &str) -> bool {
    let hash = library::sha256_hex(sealed.as_bytes());
    {
        let (set, order) = &mut *lock(&shared.seen);
        if !set.insert(hash.clone()) {
            return false;
        }
        order.push_back(hash);
        if order.len() > SEEN
            && let Some(oldest) = order.pop_front()
        {
            set.remove(&oldest);
        }
    }
    let event = format!("data: {}\n\n", serde_json::json!({ "live": sealed }));
    lock(&shared.listening).retain(|(_, tx)| tx.send(event.clone()).is_ok());
    true
}

fn live(shared: &Shared, request: &mut Request) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut sealed = String::new();
    let read = request.as_reader().take(sync::MAX_LIVE as u64 + 1).read_to_string(&mut sealed);
    if read.is_err() || sealed.len() > sync::MAX_LIVE || sealed.trim().is_empty() {
        return error(400, "that's not a live message");
    }
    if pass_on(shared, sealed.trim()) {
        let _ = lock(&shared.came).send(Came::Live(sealed.trim().to_string()));
    }
    json(200, serde_json::json!({}))
}

/// Stream the live messages to a device, on a thread of its own, for
/// `client::STREAM_FOR` (it listens again then).
fn listen(shared: &Arc<Shared>, request: Request) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<String>();
    {
        let mut listening = lock(&shared.listening);
        if listening.len() >= MOST_LISTENING {
            drop(listening);
            let _ = request.respond(error(503, "too many devices listening"));
            return;
        }
        listening.push((id, tx));
    }
    let closing = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("hub live".into()).spawn(move || {
        let until = std::time::Instant::now() + crate::library::client::STREAM_FOR;
        let mut writer = request.into_writer();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n: open\n\n";
        let mut ok = writer.write_all(head.as_bytes()).and_then(|()| writer.flush()).is_ok();
        while ok && std::time::Instant::now() < until {
            let chunk = match rx.recv_timeout(KEEP_ALIVE) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => ": keep-alive\n\n".to_string(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            ok = writer.write_all(chunk.as_bytes()).and_then(|()| writer.flush()).is_ok();
        }
        lock(&closing.listening).retain(|(listener, _)| *listener != id);
    });
    if spawned.is_err() {
        log::warn!("couldn't start a live message stream");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::client::{self, HubPut};
    use crate::song_sync::{self, Hub, HubLeaf, Moved, Relay, Room, Task};

    #[test]
    fn devices_send_songs_to_the_hub_and_take_them_from_it() {
        let root = std::env::temp_dir().join(format!("synththing-hub-server-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (me, hub_key, sync_key) = (Identity::generate().unwrap(), Identity::generate().unwrap(), [5; 32]);
        let (server, came) = HubServer::start(0, Identity::from_secret_hex(&hub_key.secret_hex()).unwrap(), &me, sync_key, root.join("hub")).unwrap();
        let base = format!("http://127.0.0.1:{}", server.addr().port());
        let key = hub_key.public();

        // It proves it's this hub; another key isn't it.
        client::hub_hello(&base, &key, true).unwrap();
        assert!(client::hub_hello(&base, &Identity::generate().unwrap().public(), true).is_err());
        let leaf = HubLeaf { device: "hub".into(), key: key.clone(), addrs: vec!["http://127.0.0.1:9".into(), base.clone()] };
        let reached = song_sync::reach(&leaf).unwrap();
        assert_eq!(reached, Hub { device: "hub".into(), base: base.clone(), key: key.clone() }, "the next address, the first not answering");

        // A song sent comes in once; again, it has it.
        let song = song_store::write_in(b"a song for the hub", "Song.mid", &root.join("desktop")).unwrap();
        let file = song_store::path_of(&root.join("desktop"), &song.key);
        assert_eq!(client::hub_put(&base, &key, &me, "desktop", &sync_key, &song.key, &file), Ok(HubPut::Taken));
        assert_eq!(came.try_recv(), Ok(Came::Song(song.key.clone())));
        assert_eq!(client::hub_put(&base, &key, &me, "desktop", &sync_key, &song.key, &file), Ok(HubPut::Has));
        // Not from someone else, nor sealed with another key.
        assert!(client::hub_put(&base, &key, &Identity::generate().unwrap(), "x", &sync_key, &song.key, &file).is_err());
        let other = song_store::write_in(b"another", "Other.mid", &root.join("desktop")).unwrap();
        assert!(client::hub_put(&base, &key, &me, "x", &[6; 32], &other.key, &song_store::path_of(&root.join("desktop"), &other.key)).is_err());
        assert!(!song_store::path_of(&root.join("hub"), &other.key).exists());

        // The phone takes it; one the hub doesn't have isn't there.
        let mut fetched = Vec::new();
        assert_eq!(client::hub_get(&base, &key, &me, "phone", &sync_key, &song.key, &mut fetched), Ok(true));
        assert_eq!(fetched, b"a song for the hub");
        assert_eq!(client::hub_get(&base, &key, &me, "phone", &sync_key, &other.key, &mut Vec::new()), Ok(false));

        // As tasks: the other song goes straight to the hub, and a phone
        // pulls it.
        let relay = |dir: &str| Relay {
            server: "http://127.0.0.1:9".into(),
            server_key: String::new(),
            identity: me.clone(),
            sync_key,
            dir: root.join(dir),
            device: dir.into(),
            hub: Some(reached.clone()),
        };
        let mut moved = Vec::new();
        let send = [Task::Send { key: other.key.clone(), to: vec!["hub".into()] }];
        song_sync::run_all(&relay("desktop"), &send, Room { song_mb: 100, full: false }, |m| {
            moved.push(m);
            true
        });
        assert_eq!(moved, [Moved::Streamed { key: other.key.clone(), to: "hub".into() }]);
        assert_eq!(song_sync::run(&relay("phone"), &Task::Pull(other.key.clone())), Moved::Fetched(other.key.clone()));
        assert!(song_store::path_of(&root.join("phone"), &other.key).exists());
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_hub_passes_live_messages_on() {
        let root = std::env::temp_dir().join(format!("synththing-hub-live-{}", std::process::id()));
        let (me, hub_key) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (server, came) = HubServer::start(0, Identity::from_secret_hex(&hub_key.secret_hex()).unwrap(), &me, [5; 32], root.clone()).unwrap();
        let base = format!("http://127.0.0.1:{}", server.addr().port());
        let key = hub_key.public();
        let (tx, heard) = mpsc::channel();
        let listening = {
            let (base, key, me) = (base.clone(), key.clone(), me.clone());
            std::thread::spawn(move || client::hub_live_listen(&base, &key, &me, "phone", |event| tx.send(event).is_ok()))
        };
        assert_eq!(heard.recv_timeout(std::time::Duration::from_secs(5)), Ok(client::StreamEvent::Opened));

        // From a device: to those listening, and to the hub itself.
        client::hub_live_send(&base, &key, &me, "desktop", "sealed-1").unwrap();
        assert_eq!(heard.recv_timeout(std::time::Duration::from_secs(5)), Ok(client::StreamEvent::Live("sealed-1".into())));
        assert_eq!(came.recv_timeout(std::time::Duration::from_secs(5)), Ok(Came::Live("sealed-1".into())));
        // Back from the server: not again. Another from there: passed on.
        assert!(!server.pass_on("sealed-1"));
        assert!(server.pass_on("sealed-2"));
        assert_eq!(heard.recv_timeout(std::time::Duration::from_secs(5)), Ok(client::StreamEvent::Live("sealed-2".into())));
        assert!(client::hub_live_send(&base, &key, &Identity::generate().unwrap(), "x", "sealed-3").is_err(), "only the user's devices");
        drop(server);
        drop(heard);
        let _ = listening;
        let _ = std::fs::remove_dir_all(&root);
    }

}
