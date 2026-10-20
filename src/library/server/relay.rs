//! The song relay (`library/sync.rs`): sealed song files one device
//! leaves for another signed in as the same identity, each by an ID only
//! those devices can tie to a song. On with sync. A file stays until a
//! device deletes it or `RELAY_DAYS` pass, within `relay_song_mb` each and
//! `relay_mb_per_identity` in all. The files are in the data folder's
//! `relay`; a rotation moves them to the new key, and deleting the
//! identity's data deletes them.

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

/// The signer of a request about relayed file `id`, once sync is on here,
/// and its body (up to `limit`).
fn signed(state: &State, request: &mut Request, path: &str, id: &str, limit: usize) -> Result<(String, Vec<u8>), Reply> {
    if !state.config.sync {
        return Err(error(404, "this server doesn't sync"));
    }
    if !valid_relay_id(id) {
        return Err(error(404, "no such file"));
    }
    let body = read_body_or(request, limit, "that song is too big to send")?;
    let key = signer(state, request, "POST", path, &body)?.ok_or_else(|| error(401, "that needs your signature"))?;
    Ok((key, body))
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

/// `POST /api/v1/relay/{id}`, signed, the sealed file as the body: leave
/// it (replacing one by that ID).
pub(super) fn put(state: &State, request: &mut Request, id: &str) -> Reply {
    let limit = (state.config.relay_song_mb * MB) as usize;
    let (key, body) = match signed(state, request, &format!("/api/v1/relay/{id}"), id, limit) {
        Ok(found) => found,
        Err(reply) => return reply,
    };
    if body.is_empty() {
        return error(400, "that's an empty file");
    }
    sweep(state);
    let held: i64 = state
        .db()
        .query_row("SELECT COALESCE(SUM(size), 0) FROM relay WHERE key = ? AND id != ?", params![key, id], |r| r.get(0))
        .unwrap_or(0);
    if held as u64 + body.len() as u64 > state.config.relay_mb_per_identity * MB {
        return error(
            413,
            format!("there are {} MB of your songs waiting here already: they go once your devices take them", held as u64 / MB),
        );
    }
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
    let key = match signed(state, request, &format!("/api/v1/relay/{id}/get"), id, 1024) {
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
    let key = match signed(state, request, &format!("/api/v1/relay/{id}/delete"), id, 1024) {
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
