//! Intake (docs/SERVER.md, docs/feedback-intake.md): feedback waiting to be
//! filed as GitHub issues, for a scheduled Claude run to collect, when
//! `intake` is set in `server.json`. The run asks with the intake token
//! (`intake.token` in the data folder, made on first run), files the notes
//! and then says which it has taken, so a run that fails partway loses
//! none: until they're taken, the next run gets them again.
//!
//! What came of them comes back the same way (`resolved`): fixed or added
//! in a release (the release workflow says so once it's out), or not
//! planned (intake says so when a note asks for something already turned
//! down). Whoever asked to hear is told.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tiny_http::Request;

use super::{error, json, notifications, now, read_body, sha256_hex, Reply, State};

pub(super) const SCHEMA_V14: &str = "
BEGIN;
ALTER TABLE feedback ADD COLUMN pushed INTEGER;
PRAGMA user_version = 14;
COMMIT;
";

pub(super) const SCHEMA_V15: &str = "
BEGIN;
ALTER TABLE feedback RENAME COLUMN pushed TO taken;
PRAGMA user_version = 15;
COMMIT;
";

/// The most notes one collection hands out.
const BATCH: i64 = 20;
/// How much of the end of a note's log goes with it.
const LOG_TAIL: usize = 8 * 1024;

/// A note waiting to be filed: no address, and only the end of its log.
#[derive(Serialize)]
struct Waiting {
    id: i64,
    created: i64,
    channel: String,
    app_version: String,
    os: String,
    text: String,
    log: Option<String>,
    performance: Option<String>,
}

#[derive(Deserialize)]
struct Taken {
    feedback: Vec<i64>,
}

/// What came of some notes: `outcome` is `fixed` or `added` (in
/// `version`), or `not_planned`.
#[derive(Deserialize)]
struct Resolved {
    feedback: Vec<i64>,
    outcome: String,
    version: Option<String>,
}

/// How much of a note its sender is reminded of.
const PREVIEW: usize = 80;

/// The request carries the intake token (and intake is on).
pub(super) fn authorized(state: &State, request: &Request) -> Result<(), Reply> {
    let Some(token) = &state.intake_token else { return Err(error(404, "intake is off")) };
    let given = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
        .unwrap_or_default();
    // (Compared as hashes, so the time taken says nothing about the token.)
    if sha256_hex(given.trim().as_bytes()) == sha256_hex(token.as_bytes()) {
        Ok(())
    } else {
        Err(error(401, "that's not the intake token"))
    }
}

/// `GET /api/v1/intake`: the oldest notes not taken yet (nor dealt with).
pub(super) fn waiting(state: &State, request: &Request) -> Reply {
    if let Err(reply) = authorized(state, request) {
        return reply;
    }
    let db = state.db();
    let notes = db
        .prepare(
            "SELECT id, created, channel, app_version, os, text, log, performance FROM feedback
             WHERE taken IS NULL AND handled IS NULL ORDER BY id LIMIT ?",
        )
        .and_then(|mut s| {
            s.query_map([BATCH], |r| {
                Ok(Waiting {
                    id: r.get(0)?,
                    created: r.get(1)?,
                    channel: r.get(2)?,
                    app_version: r.get(3)?,
                    os: r.get(4)?,
                    text: r.get(5)?,
                    log: r.get::<_, Option<String>>(6)?.map(|log| tail(&log)),
                    performance: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        });
    match notes {
        Ok(notes) => json(200, &serde_json::json!({ "feedback": notes })),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `POST /api/v1/intake/taken`: `{"feedback": [ids]}` are filed.
pub(super) fn taken(state: &State, request: &mut Request) -> Reply {
    if let Err(reply) = authorized(state, request) {
        return reply;
    }
    let body = match read_body(request, 64 * 1024) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let taken: Taken = match serde_json::from_slice(&body) {
        Ok(taken) => taken,
        Err(e) => return error(400, format!("not a list of feedback: {e}")),
    };
    let db = state.db();
    let mut marked = 0;
    for id in &taken.feedback {
        match db.execute("UPDATE feedback SET taken = ? WHERE id = ? AND taken IS NULL", params![now(), id]) {
            Ok(n) => marked += n,
            Err(e) => return error(500, format!("database: {e}")),
        }
    }
    println!("intake took {marked} feedback");
    json(200, &serde_json::json!({ "taken": marked }))
}

/// `POST /api/v1/intake/resolved`: tell the senders of `feedback` who
/// asked to hear (a `feedback_fixed`, `feedback_added` or
/// `feedback_not_planned` notification), once: then their key goes, and
/// the note counts as dealt with.
pub(super) fn resolved(state: &State, request: &mut Request) -> Reply {
    if let Err(reply) = authorized(state, request) {
        return reply;
    }
    let body = match read_body(request, 64 * 1024) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let resolved: Resolved = match serde_json::from_slice(&body) {
        Ok(resolved) => resolved,
        Err(e) => return error(400, format!("not what came of some feedback: {e}")),
    };
    let version = resolved.version.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let kind = match (resolved.outcome.as_str(), version) {
        ("fixed", Some(_)) => "feedback_fixed",
        ("added", Some(_)) => "feedback_added",
        ("fixed" | "added", None) => return error(400, "say which version it's in"),
        ("not_planned", _) => "feedback_not_planned",
        (other, _) => return error(400, format!("\"{other}\" isn't an outcome (fixed, added or not_planned)")),
    };
    let db = state.db();
    let mut told = 0;
    for id in &resolved.feedback {
        let waiting = db
            .query_row("SELECT reply_key, text FROM feedback WHERE id = ? AND reply_key IS NOT NULL", [id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .ok();
        let Some((key, text)) = waiting else { continue };
        let preview: String = text.chars().take(PREVIEW).collect();
        let mut data = serde_json::json!({ "feedback": id, "preview": preview });
        if let Some(version) = version {
            data["version"] = version.into();
        }
        notifications::notify(state, &db, &key, kind, data, false);
        if let Err(e) = db.execute(
            "UPDATE feedback SET reply_key = NULL, handled = COALESCE(handled, ?) WHERE id = ?",
            params![now(), id],
        ) {
            return error(500, format!("database: {e}"));
        }
        told += 1;
    }
    println!("told {told} feedback senders: {}", resolved.outcome);
    json(200, &serde_json::json!({ "told": told }))
}

/// The end of a log, at most `LOG_TAIL` bytes of it.
fn tail(log: &str) -> String {
    let mut start = log.len().saturating_sub(LOG_TAIL);
    while !log.is_char_boundary(start) {
        start += 1;
    }
    log[start..].to_string()
}
