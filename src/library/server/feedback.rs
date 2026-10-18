//! Feedback (docs/SERVER.md): a note about the app, from anyone, unsigned,
//! for the server's admins. It says which build it came from; a branch
//! build's feedback goes once that branch's test build is gone (the branch
//! was merged or dropped), since what it was about went with it.
//!
//! Limited per address and refused from banned ones, like reports.
//! Addresses go after 30 days, and feedback 90 days after it's dealt with.

use std::time::Duration;

use rusqlite::{params, Connection};
use tiny_http::Request;

use super::moderation::{limit, refuse_banned};
use super::{error, json, notifications, now, read_body, Reply, State};
use crate::library::{Feedback, FeedbackRequest, MAX_FEEDBACK_LOG, MAX_FEEDBACK_TEXT};

pub(super) const SCHEMA_V13: &str = "
BEGIN;
CREATE TABLE feedback (
    id INTEGER PRIMARY KEY,
    text TEXT NOT NULL,
    app_version TEXT NOT NULL,
    channel TEXT NOT NULL,
    os TEXT NOT NULL,
    log TEXT,
    ip TEXT,
    created INTEGER NOT NULL,
    handled INTEGER,
    note TEXT
);
PRAGMA user_version = 13;
COMMIT;
";

/// Feedback one address may send in a day.
const FEEDBACK_PER_DAY: usize = 10;
/// How long addresses are kept, and feedback once it's dealt with.
const KEEP_ADDRESSES: i64 = 30 * 24 * 60 * 60;
const KEEP_HANDLED: i64 = 90 * 24 * 60 * 60;

/// `POST /api/v1/feedback`.
pub(super) fn send(state: &State, request: &mut Request, ip: &str) -> Reply {
    if let Err(reply) = refuse_banned(state, None, ip)
        .and_then(|()| limit(state, "feedback", ip, FEEDBACK_PER_DAY, Duration::from_secs(24 * 3600), "feedback"))
    {
        return reply;
    }
    let body = match read_body(request, MAX_FEEDBACK_TEXT * 4 + MAX_FEEDBACK_LOG + 4096) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let feedback: FeedbackRequest = match serde_json::from_slice(&body) {
        Ok(feedback) => feedback,
        Err(e) => return error(400, format!("not feedback: {e}")),
    };
    let text = feedback.text.trim();
    if text.is_empty() {
        return error(400, "the feedback is empty");
    }
    if text.chars().count() > MAX_FEEDBACK_TEXT {
        return error(400, format!("feedback can be up to {MAX_FEEDBACK_TEXT} characters"));
    }
    if feedback.log.as_ref().is_some_and(|log| log.len() > MAX_FEEDBACK_LOG) {
        return error(400, "the log is too long");
    }
    // (Short fields, trimmed to a sane length rather than refused.)
    let short = |s: &str| s.trim().chars().take(100).collect::<String>();
    let db = state.db();
    let stored = db.execute(
        "INSERT INTO feedback (text, app_version, channel, os, log, ip, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![text, short(&feedback.app_version), short(&feedback.channel), short(&feedback.os), feedback.log, ip, now()],
    );
    match stored {
        Ok(_) => {
            let number = db.last_insert_rowid();
            println!("feedback #{number} from {} ({})", short(&feedback.channel), short(&feedback.app_version));
            let preview: String = text.chars().take(80).collect();
            notifications::notify_admins(
                state,
                &db,
                "new_feedback",
                serde_json::json!({ "feedback": number, "channel": short(&feedback.channel), "preview": preview }),
            );
            drop(db);
            super::triage::wake(state);
            json(201, &serde_json::json!({ "feedback": number }))
        }
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// Feedback, newest first: not dealt with yet (or all).
pub(super) fn list(db: &Connection, all: bool) -> rusqlite::Result<Vec<Feedback>> {
    db.prepare(
        "SELECT id, text, app_version, channel, os, log, created, handled, note FROM feedback
         WHERE ?1 OR handled IS NULL ORDER BY id DESC LIMIT 500",
    )?
    .query_map([all], |r| {
        Ok(Feedback {
            id: r.get(0)?,
            request: FeedbackRequest {
                text: r.get(1)?,
                app_version: r.get(2)?,
                channel: r.get(3)?,
                os: r.get(4)?,
                log: r.get(5)?,
            },
            created: r.get(6)?,
            handled: r.get(7)?,
            note: r.get(8)?,
        })
    })?
    .collect()
}

/// Mark feedback dealt with: how many were (0 or 1).
pub(super) fn done(db: &Connection, id: i64, note: &str) -> rusqlite::Result<usize> {
    db.execute("UPDATE feedback SET handled = ?, note = ? WHERE id = ? AND handled IS NULL", params![now(), note, id])
}

/// Forget old addresses and feedback dealt with long ago.
pub(super) fn forget_old(state: &State) {
    let now = now();
    let result = state.db().execute_batch(&format!(
        "UPDATE feedback SET ip = NULL WHERE created < {} AND ip IS NOT NULL;
         DELETE FROM feedback WHERE handled < {};",
        now - KEEP_ADDRESSES,
        now - KEEP_HANDLED
    ));
    if let Err(e) = result {
        println!("couldn't forget old feedback: {e}");
    }
}

/// Delete the feedback from branch builds whose test build is gone: those
/// not in `channels` (the `branch-<name>`s that still have one).
pub(super) fn forget_branches_except(state: &State, channels: &[String]) {
    let db = state.db();
    let gone: rusqlite::Result<usize> = db
        .prepare("SELECT DISTINCT channel FROM feedback WHERE channel LIKE 'branch-%'")
        .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>())
        .and_then(|stored| {
            let mut deleted = 0;
            for channel in stored.iter().filter(|c| !channels.contains(c)) {
                deleted += db.execute("DELETE FROM feedback WHERE channel = ?", [channel])?;
            }
            Ok(deleted)
        });
    match gone {
        Ok(0) => {}
        Ok(n) => println!("deleted {n} feedback from branch builds that are gone"),
        Err(e) => println!("couldn't forget feedback from gone branches: {e}"),
    }
}

/// `forget_branches_except`, asking GitHub which test builds there are
/// (and keeping everything if it can't say).
pub(super) fn forget_gone_branches(state: &State) {
    match crate::updater::branch_channels() {
        Ok(channels) => forget_branches_except(state, &channels),
        Err(e) => println!("couldn't list the branch builds, so kept their feedback: {e:#}"),
    }
}
