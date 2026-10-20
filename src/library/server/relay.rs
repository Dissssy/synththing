//! The song relay (`library/sync.rs`): sealed songs going between devices
//! signed in as the same identity, each by an ID only those devices can tie
//! to a song. On with sync.
//!
//! A device online listens for songs (`listen`, by an inbox ID only its
//! devices can tie to it), and one sending it a song streams it straight
//! through (`send`): kept nowhere, of any size. One that isn't listening
//! gets its songs left here instead: a file stays until a device deletes it
//! or `RELAY_DAYS` pass, within `relay_song_mb` each and
//! `relay_mb_per_identity` in all (past which the oldest go; devices ask how
//! much room is left first, so they wait instead). The files are in the
//! data folder's `relay`; a rotation moves them to the new key, and
//! deleting the identity's data deletes them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{params, OptionalExtension};
use tiny_http::{Request, Response};

use super::{error, header, json, now, read_body_or, signer, Reply, State};
use crate::library::sync::valid_relay_id;
use crate::library::RELAY_ID_HEADER;

pub(super) const SCHEMA_V17: &str = "
BEGIN;
CREATE TABLE relay (
    key TEXT NOT NULL,              -- whose
    id TEXT NOT NULL,
    file TEXT NOT NULL,             -- in the data folder's relay
    size INTEGER NOT NULL,
    created INTEGER NOT NULL,
    PRIMARY KEY (key, id)
);
PRAGMA user_version = 17;
COMMIT;
";

/// How long a file waits to be taken.
const RELAY_DAYS: i64 = 7;
const MB: u64 = 1024 * 1024;

fn dir(state: &State) -> PathBuf {
    state.data.join("relay")
}

/// The signer of a request to the relay, signed over `body` (a song's
/// isn't: it's sealed with the sync key, so it can't be changed unseen),
/// once sync is on here.
fn signed(state: &State, request: &Request, path: &str, body: &[u8]) -> Result<String, Reply> {
    if !state.config.sync {
        return Err(error(404, "this server doesn't sync"));
    }
    signer(state, request, "POST", path, body)?.ok_or_else(|| error(401, "that needs your signature"))
}

/// `signed`, about relayed file `id`, with no body.
fn signed_about(state: &State, request: &Request, path: &str, id: &str) -> Result<String, Reply> {
    if !valid_relay_id(id) {
        return Err(error(404, "no such file"));
    }
    signed(state, request, path, &[])
}

/// Forget files that waited too long, and delete files nothing names (a
/// file whose row went with a rotation or a deletion).
fn sweep(state: &State) {
    let names: Vec<String> = {
        let db = state.db();
        let _ = db.execute("DELETE FROM relay WHERE created < ?", [now() - RELAY_DAYS * 24 * 3600]);
        db.prepare("SELECT file FROM relay").and_then(|mut s| s.query_map([], |r| r.get(0))?.collect()).unwrap_or_default()
    };
    let Ok(entries) = std::fs::read_dir(dir(state)) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !names.contains(&name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `POST /api/v1/relay/room`, signed: how much more of the signer's files
/// fit (bytes), so a device sends no faster than the others take them.
pub(super) fn room(state: &State, request: &Request) -> Reply {
    let key = match signed(state, request, "/api/v1/relay/room", &[]) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    let held: Result<i64, _> = state.db().query_row(
        "SELECT COALESCE(SUM(size), 0) FROM relay WHERE key = ? AND created >= ?",
        params![key, now() - RELAY_DAYS * 24 * 3600],
        |r| r.get(0),
    );
    match held {
        Ok(held) => json(200, &serde_json::json!({ "room": (state.config.relay_mb_per_identity * MB).saturating_sub(held as u64) })),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `POST /api/v1/relay/{id}`, signed, the sealed file as the body: leave
/// it (replacing one by that ID).
pub(super) fn put(state: &State, request: &mut Request, id: &str) -> Reply {
    let key = match signed_about(state, request, &format!("/api/v1/relay/{id}"), id) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    let body = match read_body_or(request, (state.config.relay_song_mb * MB) as usize, "that song is too big to send") {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    if body.is_empty() {
        return error(400, "that's an empty file");
    }
    if body.len() as u64 > state.config.relay_mb_per_identity * MB {
        return error(413, "that song is too big to send");
    }
    // Make room by forgetting the oldest waiting: a device that wanted one
    // finds it gone, and it's sent again.
    {
        let db = state.db();
        let waiting: Vec<(String, i64)> = db
            .prepare("SELECT id, size FROM relay WHERE key = ? AND id != ? ORDER BY created DESC")
            .and_then(|mut s| s.query_map(params![key, id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
            .unwrap_or_default();
        let mut held = body.len() as u64;
        for (old, size) in waiting {
            held += size as u64;
            if held > state.config.relay_mb_per_identity * MB {
                let _ = db.execute("DELETE FROM relay WHERE key = ? AND id = ?", params![key, old]);
            }
        }
    }
    sweep(state);
    let file = format!("{}.bin", crate::library::new_nonce());
    let written = std::fs::create_dir_all(dir(state)).and_then(|()| std::fs::write(dir(state).join(&file), &body));
    if let Err(e) = written {
        return error(500, format!("couldn't keep the file: {e}"));
    }
    let db = state.db();
    let stored = db.execute(
        "INSERT OR REPLACE INTO relay (key, id, file, size, created) VALUES (?, ?, ?, ?, ?)",
        params![key, id, file, body.len() as i64, now()],
    );
    drop(db);
    match stored {
        // (A file it replaced goes with the next sweep.)
        Ok(_) => json(200, &serde_json::json!({})),
        Err(e) => {
            let _ = std::fs::remove_file(dir(state).join(&file));
            error(500, format!("database: {e}"))
        }
    }
}

/// `POST /api/v1/relay/{id}/get`, signed: the sealed file.
pub(super) fn get(state: &State, request: &Request, id: &str) -> Reply {
    let key = match signed_about(state, request, &format!("/api/v1/relay/{id}/get"), id) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    let file: Option<String> = state
        .db()
        .query_row("SELECT file FROM relay WHERE key = ? AND id = ? AND created >= ?", params![key, id, now() - RELAY_DAYS * 24 * 3600], |r| r.get(0))
        .optional()
        .unwrap_or(None);
    match file.and_then(|file| std::fs::read(dir(state).join(file)).ok()) {
        Some(bytes) => Response::from_data(bytes).with_header(header("Content-Type", "application/octet-stream")),
        None => error(404, "that song isn't waiting here (any more)"),
    }
}

/// `POST /api/v1/relay/{id}/delete`, signed: it's been taken.
pub(super) fn delete(state: &State, request: &Request, id: &str) -> Reply {
    let key = match signed_about(state, request, &format!("/api/v1/relay/{id}/delete"), id) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    let deleted = state.db().execute("DELETE FROM relay WHERE key = ? AND id = ?", params![key, id]);
    match deleted {
        Ok(_) => {
            sweep(state);
            json(200, &serde_json::json!({}))
        }
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// How long a device listening for songs is held waiting for one.
const INBOX_WAIT: Duration = Duration::from_secs(40);
/// A device that listened this recently still counts as listening: it's
/// between songs, about to listen again.
const LISTENED: Duration = Duration::from_secs(15);
/// How long a song sent to a device listening waits for it to be ready
/// for it (taking another song, or between songs).
const SEND_WAIT: Duration = Duration::from_secs(30);
/// Devices listening, and songs waiting to be streamed, at once: in all,
/// and of one key's.
const MOST_INBOXES: usize = 256;
const INBOXES_PER_KEY: usize = 8;

/// The devices listening for songs, by key and inbox ID.
#[derive(Default)]
pub(super) struct Inboxes {
    by: HashMap<(String, String), Inbox>,
    /// Threads holding a request: listening, or a song waiting for one.
    held: usize,
    generation: u64,
}

struct Inbox {
    /// Its request held waiting for a song (by generation, the newest).
    waiting: Option<(u64, Sender<(String, Request)>)>,
    /// A song is streaming to it.
    busy: bool,
    seen: Instant,
}

impl Inbox {
    fn listening(&self) -> bool {
        self.waiting.is_some() || self.busy || self.seen.elapsed() < LISTENED
    }
}

fn inboxes(state: &State) -> std::sync::MutexGuard<'_, Inboxes> {
    state.inboxes.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether `key` may hold another request: room for it is taken if so.
fn hold(inboxes: &mut Inboxes, key: &str) -> bool {
    // (Forgetting devices that stopped listening.)
    inboxes.by.retain(|_, inbox| inbox.listening());
    let mine = inboxes.by.iter().filter(|((k, _), inbox)| k == key && inbox.waiting.is_some()).count();
    if inboxes.held >= MOST_INBOXES || mine >= INBOXES_PER_KEY {
        return false;
    }
    inboxes.held += 1;
    true
}

/// `POST /api/v1/relay/inbox/{inbox}`, signed: listen for a song streamed
/// to this device, for up to `INBOX_WAIT` (204 if none came), on a thread
/// of its own. One comes with its ID in `RELAY_ID_HEADER`.
pub(super) fn listen(state: &Arc<State>, request: Request, inbox: &str) {
    let key = match signed_about(state, &request, &format!("/api/v1/relay/inbox/{inbox}"), inbox) {
        Ok(key) => key,
        Err(reply) => {
            let _ = request.respond(reply);
            return;
        }
    };
    let slot = (key, inbox.to_string());
    let (tx, rx) = mpsc::channel();
    let generation = {
        let mut inboxes = inboxes(state);
        if !hold(&mut inboxes, &slot.0) {
            drop(inboxes);
            let _ = request.respond(error(503, "too many devices listening for songs: try again later"));
            return;
        }
        inboxes.generation += 1;
        let generation = inboxes.generation;
        // (One it replaces stops waiting.)
        let inbox = inboxes.by.entry(slot.clone()).or_insert(Inbox { waiting: None, busy: false, seen: Instant::now() });
        inbox.waiting = Some((generation, tx));
        inbox.seen = Instant::now();
        generation
    };
    let shared = Arc::clone(state);
    // It's done waiting, or taking a song.
    let done = move || {
        let mut inboxes = inboxes(&shared);
        if let Some(inbox) = inboxes.by.get_mut(&slot) {
            if inbox.waiting.as_ref().is_some_and(|(g, _)| *g == generation) {
                inbox.waiting = None;
            }
            inbox.busy = false;
            inbox.seen = Instant::now();
        }
    };
    let mine = Arc::clone(state);
    let spawned = std::thread::Builder::new().name("relay inbox".into()).spawn(move || {
        let got = match rx.recv_timeout(INBOX_WAIT) {
            Ok(got) => Some(got),
            // (Or one handed over just as it stopped waiting.)
            Err(_) => {
                done();
                rx.try_recv().ok()
            }
        };
        match got {
            Some((id, mut from)) => {
                let length = from.body_length();
                let headers = vec![
                    header("Content-Type", "application/octet-stream"),
                    header(RELAY_ID_HEADER, &id),
                    header("X-Accel-Buffering", "no"),
                ];
                let streamed = request.respond(Response::new(200.into(), headers, from.as_reader(), length, None));
                done();
                let _ = from.respond(match streamed {
                    Ok(()) => json(200, &serde_json::json!({})),
                    Err(_) => error(502, "the other device stopped taking the song"),
                });
            }
            None => {
                let _ = request.respond(Response::empty(204));
            }
        }
        inboxes(&mine).held -= 1;
    });
    if spawned.is_err() {
        inboxes(state).held -= 1;
    }
}

/// `POST /api/v1/relay/listening`, signed, `{"inboxes": [...]}`: which of
/// them are listening for songs.
pub(super) fn listening(state: &State, request: &mut Request) -> Reply {
    let body = match read_body_or(request, 64 * 1024, "that's too many devices") {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let key = match signed(state, request, "/api/v1/relay/listening", &body) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    #[derive(serde::Deserialize)]
    struct Ask {
        inboxes: Vec<String>,
    }
    let Ok(ask) = serde_json::from_slice::<Ask>(&body) else {
        return error(400, "that's not a list of devices");
    };
    let inboxes = inboxes(state);
    let listening: Vec<&String> =
        ask.inboxes.iter().filter(|inbox| inboxes.by.get(&(key.clone(), inbox.to_string())).is_some_and(Inbox::listening)).collect();
    json(200, &serde_json::json!({ "listening": listening }))
}

/// `POST /api/v1/relay/{id}/send/{inbox}`, signed, the sealed song as the
/// body: stream it straight to the device listening as `inbox` (once it's
/// ready for it, for up to `SEND_WAIT`), on a thread of its own. 200 once
/// it took it all; 404 if it isn't listening.
pub(super) fn send(state: &Arc<State>, request: Request, id: &str, inbox: &str) {
    let path = format!("/api/v1/relay/{id}/send/{inbox}");
    let key = match signed_about(state, &request, &path, id) {
        Ok(_) if !valid_relay_id(inbox) => Err(error(404, "no such device")),
        signed => signed,
    };
    let key = match key {
        Ok(key) => key,
        Err(reply) => {
            let _ = request.respond(reply);
            return;
        }
    };
    if !hold(&mut inboxes(state), &key) {
        let _ = request.respond(error(503, "too many songs streaming: try again later"));
        return;
    }
    let slot = (key, inbox.to_string());
    let (id, shared) = (id.to_string(), Arc::clone(state));
    let spawned = std::thread::Builder::new().name("relay send".into()).spawn(move || {
        let deadline = Instant::now() + SEND_WAIT;
        let mut request = request;
        loop {
            let waiting = {
                let mut inboxes = inboxes(&shared);
                match inboxes.by.get_mut(&slot) {
                    Some(inbox) if inbox.waiting.is_some() => {
                        inbox.busy = true;
                        inbox.waiting.take()
                    }
                    Some(inbox) if inbox.listening() && Instant::now() < deadline => None,
                    _ => break,
                }
            };
            match waiting {
                Some((_, tx)) => match tx.send((id.clone(), request)) {
                    Ok(()) => {
                        inboxes(&shared).held -= 1;
                        return;
                    }
                    // (It stopped waiting just then.)
                    Err(mpsc::SendError((_, back))) => {
                        request = back;
                        if let Some(inbox) = inboxes(&shared).by.get_mut(&slot) {
                            inbox.busy = false;
                        }
                    }
                },
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        inboxes(&shared).held -= 1;
        let _ = request.respond(error(404, "that device isn't listening"));
    });
    if spawned.is_err() {
        inboxes(state).held -= 1;
    }
}
