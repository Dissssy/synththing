//! Sync (`library/sync.rs`): each identity's sealed blobs, by name, with
//! their revisions. On with `"sync": true`. A rotation moves them to the
//! new key; deleting the identity's data deletes them.

use rusqlite::{params, OptionalExtension};
use tiny_http::Request;

use super::{error, json, notifications, now, read_body_or, signer, Reply, State};
use crate::library::sync::{valid_name, Blob, Changed, ChangedEvent, LiveEvent, Write, Written, MAX_BLOBS, MAX_DATA, MAX_LIVE, MAX_TOTAL};

/// The song library before it was split over shards (`library-0`, ...),
/// which nothing reads any more.
pub(super) const SCHEMA_V24: &str = "
BEGIN;
DELETE FROM sync WHERE name = 'library';
PRAGMA user_version = 24;
COMMIT;
";

pub(super) const SCHEMA_V16: &str = "
BEGIN;
CREATE TABLE sync (
    key TEXT NOT NULL,              -- whose
    name TEXT NOT NULL,
    revision INTEGER NOT NULL,
    data TEXT NOT NULL,             -- sealed with their sync key, base64
    updated INTEGER NOT NULL,
    PRIMARY KEY (key, name)
);
PRAGMA user_version = 16;
COMMIT;
";

/// The signer of a request for blob `name`, once sync is on here.
fn signed(state: &State, request: &mut Request, path: &str, name: &str, limit: usize) -> Result<(String, Vec<u8>), Reply> {
    if !state.config.sync {
        return Err(error(404, "this server doesn't sync"));
    }
    if !valid_name(name) {
        return Err(error(404, "no such synced data"));
    }
    let body = read_body_or(request, limit, "that's too much to sync")?;
    let key = signer(state, request, "POST", path, &body)?.ok_or_else(|| error(401, "that needs your signature"))?;
    Ok((key, body))
}

/// `POST /api/v1/live`, signed: a sealed message (`LiveEvent`) passed to
/// the signer's open notification streams, its own devices', and never
/// kept. Answers how many streams it reached.
pub(super) fn live(state: &State, request: &mut Request) -> Reply {
    if !state.config.sync {
        return error(404, "this server doesn't sync");
    }
    let body = match read_body_or(request, MAX_LIVE + 1024, "that's too big a live message") {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let key = match signer(state, request, "POST", "/api/v1/live", &body) {
        Ok(Some(key)) => key,
        Ok(None) => return error(401, "that needs your signature"),
        Err(reply) => return reply,
    };
    let event: LiveEvent = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(e) => return error(400, format!("not understood: {e}")),
    };
    if event.live.is_empty() || event.live.len() > MAX_LIVE {
        return error(400, "the live message is empty or too big");
    }
    let reached = notifications::push(state, &key, &serde_json::to_string(&event).unwrap_or_default());
    json(200, &serde_json::json!({ "reached": reached }))
}

/// `POST /api/v1/sync/{name}`, signed: write the blob, if it was made
/// from the revision the server has.
pub(super) fn write(state: &State, request: &mut Request, name: &str) -> Reply {
    let (key, body) = match signed(state, request, &format!("/api/v1/sync/{name}"), name, MAX_DATA + 1024) {
        Ok(found) => found,
        Err(reply) => return reply,
    };
    let write: Write = match serde_json::from_slice(&body) {
        Ok(write) => write,
        Err(e) => return error(400, format!("not understood: {e}")),
    };
    if write.data.is_empty() || write.data.len() > MAX_DATA {
        return error(400, "the synced data is empty or too big");
    }
    let db = state.db();
    let written = (|| -> rusqlite::Result<Result<Written, String>> {
        let tx = db.unchecked_transaction()?;
        let current: Option<i64> =
            tx.query_row("SELECT revision FROM sync WHERE key = ? AND name = ?", params![key, name], |r| r.get(0)).optional()?;
        if current.unwrap_or(0) != write.base {
            return Ok(Ok(Written { stored: false, revision: current.unwrap_or(0) }));
        }
        let (blobs, others): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(data)), 0) FROM sync WHERE key = ? AND name != ?",
            params![key, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if current.is_none() && blobs >= MAX_BLOBS as i64 {
            return Ok(Err(format!("there's a limit of {MAX_BLOBS} kinds of synced data")));
        }
        if others as usize + write.data.len() > MAX_TOTAL {
            return Ok(Err(format!("there's a limit of {} MB of synced data in all", MAX_TOTAL / (1024 * 1024))));
        }
        let revision = write.base + 1;
        tx.execute(
            "INSERT OR REPLACE INTO sync (key, name, revision, data, updated) VALUES (?, ?, ?, ?, ?)",
            params![key, name, revision, write.data, now()],
        )?;
        tx.commit()?;
        Ok(Ok(Written { stored: true, revision }))
    })();
    drop(db);
    match written {
        Ok(Ok(written)) => {
            if written.stored {
                let event = ChangedEvent { sync: Changed { name: name.to_string(), revision: written.revision } };
                notifications::push(state, &key, &serde_json::to_string(&event).unwrap_or_default());
            }
            json(200, &written)
        }
        Ok(Err(limit)) => error(400, limit),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `POST /api/v1/sync/{name}/get`, signed: the blob and its revision.
pub(super) fn get(state: &State, request: &mut Request, name: &str) -> Reply {
    let key = match signed(state, request, &format!("/api/v1/sync/{name}/get"), name, 1024) {
        Ok((key, _)) => key,
        Err(reply) => return reply,
    };
    let found = state
        .db()
        .query_row("SELECT revision, data FROM sync WHERE key = ? AND name = ?", params![key, name], |r| {
            Ok(Blob { revision: r.get(0)?, data: Some(r.get(1)?) })
        })
        .optional();
    match found {
        Ok(blob) => json(200, &blob.unwrap_or_default()),
        Err(e) => error(500, format!("database: {e}")),
    }
}
