//! The song relay (`library/sync.rs`): sealed song files one device
//! leaves for another signed in as the same identity, each by an ID only
//! those devices can tie to a song. On with sync. A file stays until a
//! device deletes it or `RELAY_DAYS` pass, within `relay_song_mb` each and
//! `relay_mb_per_identity` in all (past which the oldest go; devices ask how
//! much room is left first, so they wait instead). The files are
//! in the data folder's `relay`; a rotation moves them to the new key, and
//! deleting the identity's data deletes them.

use std::path::PathBuf;

use rusqlite::{params, OptionalExtension};
use tiny_http::{Request, Response};

use super::{error, header, json, now, read_body_or, signer, Reply, State};
use crate::library::sync::valid_relay_id;

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

/// The signer of a request to the relay, once sync is on here, and its
/// body (up to `limit`).
fn signed(state: &State, request: &mut Request, path: &str, limit: usize) -> Result<(String, Vec<u8>), Reply> {
    if !state.config.sync {
        return Err(error(404, "this server doesn't sync"));
    }
    let body = read_body_or(request, limit, "that song is too big to send")?;
    let key = signer(state, request, "POST", path, &body)?.ok_or_else(|| error(401, "that needs your signature"))?;
    Ok((key, body))
}

/// `signed`, about relayed file `id`.
fn signed_about(state: &State, request: &mut Request, path: &str, id: &str, limit: usize) -> Result<(String, Vec<u8>), Reply> {
    if !valid_relay_id(id) {
        return Err(error(404, "no such file"));
    }
    signed(state, request, path, limit)
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
pub(super) fn room(state: &State, request: &mut Request) -> Reply {
    let key = match signed(state, request, "/api/v1/relay/room", 1024) {
        Ok((key, _)) => key,
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
    let limit = (state.config.relay_song_mb * MB) as usize;
    let (key, body) = match signed_about(state, request, &format!("/api/v1/relay/{id}"), id, limit) {
        Ok(found) => found,
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
pub(super) fn get(state: &State, request: &mut Request, id: &str) -> Reply {
    let key = match signed_about(state, request, &format!("/api/v1/relay/{id}/get"), id, 1024) {
        Ok((key, _)) => key,
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
pub(super) fn delete(state: &State, request: &mut Request, id: &str) -> Reply {
    let key = match signed_about(state, request, &format!("/api/v1/relay/{id}/delete"), id, 1024) {
        Ok((key, _)) => key,
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
