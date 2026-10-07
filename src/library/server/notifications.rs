//! Notifications (docs/SERVER.md): what happened to someone's things,
//! kept for them by key: milestones, moderation, remixes, their reports
//! handled; and for moderators, what's new in the queue.
//!
//! A notification is a kind and its fields, never wording: the app words
//! each kind itself (its own templates), and the template stored here is
//! only for an app that doesn't know the kind yet. Fields of a kind only
//! ever gain more. They're listed (`POST /api/v1/notifications`, signed)
//! and pushed while the app's open (`GET /api/v1/notifications/stream`,
//! signed, Server-Sent Events), and marked read or handled by the app.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{params, Connection};
use tiny_http::Request;

use super::{error, json, now, read_body, signer, Reply, State};
use crate::library::Notification;

pub(super) const SCHEMA_V10: &str = "
BEGIN;
CREATE TABLE notifications (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL,              -- whose
    kind TEXT NOT NULL,
    data TEXT NOT NULL,             -- the kind's fields, JSON
    template TEXT NOT NULL,         -- for apps that don't know the kind
    created INTEGER NOT NULL,
    read INTEGER NOT NULL DEFAULT 0,
    needs_action INTEGER NOT NULL DEFAULT 0,
    handled INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX notifications_key ON notifications(key, id);
ALTER TABLE scripts ADD COLUMN downloads INTEGER NOT NULL DEFAULT 0;
PRAGMA user_version = 10;
COMMIT;
";

/// Encores and downloads worth a notification.
const MILESTONES: [u64; 12] = [10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 50_000, 100_000];
/// Streams open at once, in all and per key.
const MOST_STREAMS: usize = 256;
const STREAMS_PER_KEY: usize = 4;
/// How long a stream goes quiet before a keep-alive comment.
const KEEP_ALIVE: Duration = Duration::from_secs(25);
/// Read notifications that needed nothing done are kept this long.
const KEEP_READ: i64 = 90 * 24 * 60 * 60;

/// The open streams, by key.
#[derive(Default)]
pub(super) struct Streams {
    senders: HashMap<String, Vec<Sender<String>>>,
}

/// Each kind's template, for apps that don't know it: `{field}`s filled in
/// from its fields.
fn template(kind: &str) -> &'static str {
    match kind {
        "encore_milestone" => "\"{name}\" has {count} encores!",
        "download_milestone" => "\"{name}\" has been installed {count} times!",
        "hidden" => "The moderators hid \"{name}\": {reason}",
        "reinstated" => "\"{name}\" is back up",
        "removed" => "The moderators deleted \"{name}\"",
        "remixed" => "{by} published a remix of \"{name}\": \"{remix_name}\"",
        "report_handled" => "Your report of \"{name}\" was dealt with: {outcome}",
        "new_report" => "New report of \"{name}\": {reason}",
        _ => "{kind}",
    }
}

/// Tell `key` about something: stored, and pushed to its open streams.
pub(super) fn notify(state: &State, db: &Connection, key: &str, kind: &str, data: serde_json::Value, needs_action: bool) {
    let template = template(kind);
    let stored = db.execute(
        "INSERT INTO notifications (key, kind, data, template, created, needs_action) VALUES (?, ?, ?, ?, ?, ?)",
        params![key, kind, data.to_string(), template, now(), needs_action],
    );
    if stored.is_err() {
        return;
    }
    let notification = Notification {
        id: db.last_insert_rowid(),
        kind: kind.to_string(),
        data,
        template: template.to_string(),
        created: now(),
        read: false,
        needs_action,
        handled: false,
    };
    let message = serde_json::to_string(&notification).unwrap_or_default();
    let mut streams = state.streams.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(senders) = streams.senders.get_mut(key) {
        senders.retain(|sender| sender.send(message.clone()).is_ok());
    }
}

/// Tell every admin (moderators' notifications).
pub(super) fn notify_admins(state: &State, db: &Connection, kind: &str, data: serde_json::Value) {
    let admins: Vec<String> = db
        .prepare("SELECT key FROM admins")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .unwrap_or_default();
    for admin in admins {
        notify(state, db, &admin, kind, data.clone(), false);
    }
}

/// The milestone `count` just reached, if it's one (`before` it was less).
fn milestone(before: u64, count: u64) -> Option<u64> {
    MILESTONES.iter().rev().copied().find(|&m| before < m && count >= m)
}

/// A script's encores changed (from `before`): tell its author if that's
/// a milestone.
pub(super) fn encores_changed(state: &State, db: &Connection, script: &str, before: u64, now_count: u64) {
    if let Some(count) = milestone(before, now_count) {
        author_notify(state, db, script, "encore_milestone", |name| serde_json::json!({ "script": script, "name": name, "count": count }));
    }
}

/// A script was downloaded: counted, and its author told at milestones.
pub(super) fn downloaded(state: &State, db: &Connection, script: &str) {
    let before: i64 = db.query_row("SELECT downloads FROM scripts WHERE id = ?", [script], |r| r.get(0)).unwrap_or(0);
    let _ = db.execute("UPDATE scripts SET downloads = downloads + 1 WHERE id = ?", [script]);
    if let Some(count) = milestone(before.max(0) as u64, before.max(0) as u64 + 1) {
        author_notify(state, db, script, "download_milestone", |name| serde_json::json!({ "script": script, "name": name, "count": count }));
    }
}

/// Tell a script's author (a signed upload's), with fields made from its
/// name.
pub(super) fn author_notify(
    state: &State,
    db: &Connection,
    script: &str,
    kind: &str,
    fields: impl FnOnce(&str) -> serde_json::Value,
) {
    let found: Option<(Option<String>, String)> =
        db.query_row("SELECT author_key, name FROM scripts WHERE id = ?", [script], |r| Ok((r.get(0)?, r.get(1)?))).ok();
    if let Some((Some(author), name)) = found {
        notify(state, db, &author, kind, fields(&name), false);
    }
}

/// Forget read notifications that needed nothing done, after a while.
pub(super) fn forget_old(state: &State) {
    let _ = state.db().execute(
        "DELETE FROM notifications WHERE read = 1 AND (needs_action = 0 OR handled = 1) AND created < ?",
        [now() - KEEP_READ],
    );
}

#[derive(serde::Deserialize, Default)]
struct ListRequest {
    /// Only those after this one.
    #[serde(default)]
    after: Option<i64>,
}

#[derive(serde::Deserialize, Default)]
struct MarkRequest {
    #[serde(default)]
    ids: Vec<i64>,
    /// Every one of the key's.
    #[serde(default)]
    all: bool,
    /// Done with (for those that need something done), not just read.
    #[serde(default)]
    handled: bool,
}

fn signed_json<T: serde::de::DeserializeOwned + Default>(state: &State, request: &mut Request, path: &str) -> Result<(String, T), Reply> {
    let body = read_body(request, 64 * 1024)?;
    let key = signer(state, request, "POST", path, &body)?.ok_or_else(|| error(401, "that needs your signature"))?;
    let value = if body.is_empty() {
        T::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| error(400, format!("not understood: {e}")))?
    };
    Ok((key, value))
}

/// `POST /api/v1/notifications`, signed: the newest 200 (after `after`).
pub(super) fn list(state: &State, request: &mut Request) -> Reply {
    let (key, ask): (String, ListRequest) = match signed_json(state, request, "/api/v1/notifications") {
        Ok(found) => found,
        Err(reply) => return reply,
    };
    let rows = state
        .db()
        .prepare(
            "SELECT id, kind, data, template, created, read, needs_action, handled FROM notifications
             WHERE key = ? AND id > ? ORDER BY id DESC LIMIT 200",
        )
        .and_then(|mut s| {
            s.query_map(params![key, ask.after.unwrap_or(0)], |r| {
                Ok(Notification {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    data: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                    template: r.get(3)?,
                    created: r.get(4)?,
                    read: r.get::<_, i64>(5)? != 0,
                    needs_action: r.get::<_, i64>(6)? != 0,
                    handled: r.get::<_, i64>(7)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        });
    match rows {
        Ok(rows) => json(200, &rows),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `POST /api/v1/notifications/mark`, signed: read (or handled).
pub(super) fn mark(state: &State, request: &mut Request) -> Reply {
    let (key, ask): (String, MarkRequest) = match signed_json(state, request, "/api/v1/notifications/mark") {
        Ok(found) => found,
        Err(reply) => return reply,
    };
    let db = state.db();
    let column = if ask.handled { "handled = 1, read = 1" } else { "read = 1" };
    let result = if ask.all {
        db.execute(&format!("UPDATE notifications SET {column} WHERE key = ?"), [&key])
    } else {
        ask.ids.iter().try_fold(0, |n, id| {
            db.execute(&format!("UPDATE notifications SET {column} WHERE key = ? AND id = ?"), params![key, id]).map(|m| n + m)
        })
    };
    match result {
        Ok(n) => json(200, &serde_json::json!({ "marked": n })),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `GET /api/v1/notifications/stream`, signed: new notifications as
/// Server-Sent Events while it's open. On a thread of its own, writing
/// (and flushing) each event straight to the connection.
pub(super) fn stream(state: &Arc<State>, request: Request) {
    let key = match signer(state, &request, "GET", "/api/v1/notifications/stream", &[]) {
        Ok(Some(key)) => key,
        Ok(None) => {
            let _ = request.respond(error(401, "that needs your signature"));
            return;
        }
        Err(reply) => {
            let _ = request.respond(reply);
            return;
        }
    };
    let (tx, rx) = mpsc::channel::<String>();
    {
        let mut streams = state.streams.lock().unwrap_or_else(|p| p.into_inner());
        let open = state.open_streams.load(Ordering::Relaxed);
        let mine = streams.senders.get(&key).map_or(0, Vec::len);
        if open >= MOST_STREAMS || mine >= STREAMS_PER_KEY {
            drop(streams);
            let _ = request.respond(error(503, "too many open notification streams: try again later"));
            return;
        }
        streams.senders.entry(key.clone()).or_default().push(tx);
        state.open_streams.fetch_add(1, Ordering::Relaxed);
    }
    let shared = Arc::clone(state);
    let spawned = std::thread::Builder::new().name("notifications".into()).spawn(move || {
        let mut writer = request.into_writer();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
                    X-Accel-Buffering: no\r\nConnection: close\r\n\r\n: open\n\n";
        let mut ok = writer.write_all(head.as_bytes()).and_then(|()| writer.flush()).is_ok();
        while ok {
            let chunk = match rx.recv_timeout(KEEP_ALIVE) {
                // (A check that the stream's still open: nothing to send.)
                Ok(message) if message.is_empty() => continue,
                Ok(message) => format!("data: {message}\n\n"),
                Err(mpsc::RecvTimeoutError::Timeout) => ": keep-alive\n\n".to_string(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            ok = writer.write_all(chunk.as_bytes()).and_then(|()| writer.flush()).is_ok();
        }
        // (Gone: its sender goes the next time one's sent to it.)
        shared.open_streams.fetch_sub(1, Ordering::Relaxed);
        drop(rx);
        let mut streams = shared.streams.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(senders) = streams.senders.get_mut(&key) {
            senders.retain(|sender| sender.send(String::new()).is_ok());
            if senders.is_empty() {
                streams.senders.remove(&key);
            }
        }
    });
    if spawned.is_err() {
        state.open_streams.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milestones() {
        assert_eq!(milestone(9, 10), Some(10));
        assert_eq!(milestone(10, 11), None);
        assert_eq!(milestone(8, 60), Some(50), "the highest crossed");
        assert_eq!(milestone(0, 3), None);
    }
}
