//! Deleting an identity's data from a server (docs/SERVER.md), as data
//! protection laws ask: its scripts, the encores it gave, its admin rights,
//! its email hash, and its key on the reports it made.
//!
//! Asked for with the key (`POST /api/v1/identity/delete`); on a server
//! with mail, where the identity has an email attached, confirmed with a
//! code sent to it too (the email has the usual "that wasn't me" link).
//! Nothing goes at once: the scripts are hidden (`hidden = 2`, deleted by
//! their author, so every listing and download leaves them out), the
//! encores set aside and the admin rights taken away, and the key is
//! refused for anything but `restore` for 30 days, which puts it all back.
//! After that, `purge` (daily) deletes it for good. Bans stay: deleting
//! is no way out of one.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use tiny_http::Request;

use super::authority::{cancel_link, check_code, email_hash, has_email, new_code, store_code};
use super::mail::code_mail;
use super::{error, json, now, read_body, remove_previews, signer_any, Reply, State};
use crate::library::key_id;

pub(super) const SCHEMA_V9: &str = "
BEGIN;
CREATE TABLE deletions (
    key TEXT PRIMARY KEY,
    requested INTEGER NOT NULL,
    purge_after INTEGER NOT NULL,
    was_admin INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE deleted_encores (script_id TEXT NOT NULL, key TEXT NOT NULL, ip TEXT, created INTEGER NOT NULL);
PRAGMA user_version = 9;
COMMIT;
";

/// How long a deletion can be undone.
const UNDO_SECONDS: i64 = 30 * 24 * 60 * 60;

/// When `key`'s pending deletion goes for good, if it has one.
pub(super) fn pending(db: &Connection, key: &str) -> Option<i64> {
    db.query_row("SELECT purge_after FROM deletions WHERE key = ?", [key], |r| r.get(0)).optional().ok().flatten()
}

#[derive(Deserialize, Default)]
struct DeleteRequest {
    /// The email attached to the identity, where a code is needed.
    #[serde(default)]
    address: Option<String>,
    /// The code it got.
    #[serde(default)]
    code: Option<String>,
}

/// A signed request's key and body (a deleted key still gets this far).
fn signed(state: &State, request: &mut Request, path: &str) -> Result<(String, DeleteRequest), Reply> {
    let body = read_body(request, 16 * 1024)?;
    let key = signer_any(state, request, "POST", path, &body)?.ok_or_else(|| error(401, "that needs your signature"))?;
    let ask = if body.is_empty() {
        DeleteRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| error(400, format!("not understood: {e}")))?
    };
    Ok((key, ask))
}

/// Mark everything of `key`'s deleted: undoable until `purge_after`.
fn mark(db: &Connection, key: &str) -> rusqlite::Result<i64> {
    let tx = db.unchecked_transaction()?;
    let purge_after = now() + UNDO_SECONDS;
    let was_admin = tx.execute("DELETE FROM admins WHERE key = ?", [key])? > 0;
    tx.execute(
        "INSERT OR REPLACE INTO deletions (key, requested, purge_after, was_admin) VALUES (?, ?, ?, ?)",
        params![key, now(), purge_after, was_admin],
    )?;
    tx.execute(
        "UPDATE scripts SET hidden = 2, hidden_reason = 'deleted by its author' WHERE author_key = ? AND hidden = 0",
        [key],
    )?;
    tx.execute("INSERT INTO deleted_encores SELECT script_id, key, ip, created FROM encores WHERE key = ?", [key])?;
    tx.execute("DELETE FROM encores WHERE key = ?", [key])?;
    tx.execute("UPDATE scripts SET encores = (SELECT COUNT(*) FROM encores WHERE script_id = scripts.id)", [])?;
    tx.execute("DELETE FROM codes WHERE key = ?", [key])?;
    tx.commit()?;
    Ok(purge_after)
}

/// `POST /api/v1/identity/delete`, signed: delete (in 30 days) what this
/// server has of the key's; first a code to the attached email, where
/// there's one and mail is on.
pub(super) fn delete(state: &State, request: &mut Request, ip: &str) -> Reply {
    let result = (|| {
        let (key, ask) = signed(state, request, "/api/v1/identity/delete")?;
        let db = state.db();
        if let Some(until) = pending(&db, &key) {
            return Ok(serde_json::json!({ "deleted": until }));
        }
        let by_email = state.mailer.on() && has_email(&db, &key);
        if by_email {
            let attached: Option<String> = db
                .query_row("SELECT email_hash FROM identities WHERE key = ?", [&key], |r| r.get(0))
                .optional()
                .map_err(|e| error(500, format!("database: {e}")))?
                .flatten();
            let paused: Option<i64> = db
                .query_row("SELECT changes_paused_until FROM identities WHERE key = ?", [&key], |r| r.get(0))
                .optional()
                .map_err(|e| error(500, format!("database: {e}")))?
                .flatten();
            if paused.is_some_and(|until| until > now()) {
                return Err(error(429, "that's paused for a day (a code for your identity was cancelled from its email)"));
            }
            let Some(address) = ask.address.as_deref().filter(|a| attached.as_deref() == Some(email_hash(state, a).as_str()))
            else {
                return Err(error(
                    403,
                    "your identity has an email attached here: type it in, and a code goes to it to confirm",
                ));
            };
            let code = new_code()?;
            let (token, link) = cancel_link(state)?;
            store_code(&db, &key, "delete", &code, None, None, Some(&token), ip)?;
            drop(db);
            let cancel = link.as_deref().map(|l| (l, "and nothing is deleted (whoever asked can't try again for a day)"));
            let mail = code_mail(address.trim(), &state.config.name, &code, "to delete your identity's data", cancel);
            state.mailer.send(mail).map_err(|e| error(502, e))?;
            return Ok(serde_json::json!({ "sent": true }));
        }
        let until = mark(&db, &key).map_err(|e| error(500, format!("database: {e}")))?;
        println!("#{}: deleted (undoable for 30 days)", key_id(&key));
        Ok(serde_json::json!({ "deleted": until }))
    })();
    match result {
        Ok(reply) => json(200, &reply),
        Err(reply) => reply,
    }
}

/// `POST /api/v1/identity/delete/confirm`, signed: the emailed code.
pub(super) fn confirm(state: &State, request: &mut Request) -> Reply {
    let result = (|| {
        let (key, ask) = signed(state, request, "/api/v1/identity/delete/confirm")?;
        let db = state.db();
        check_code(&db, &key, "delete", ask.code.as_deref().unwrap_or_default())?;
        let until = mark(&db, &key).map_err(|e| error(500, format!("database: {e}")))?;
        println!("#{}: deleted, confirmed by email (undoable for 30 days)", key_id(&key));
        Ok(until)
    })();
    match result {
        Ok(until) => json(200, &serde_json::json!({ "deleted": until })),
        Err(reply) => reply,
    }
}

/// `POST /api/v1/identity/restore`, signed: undo a deletion.
pub(super) fn restore(state: &State, request: &mut Request) -> Reply {
    let result = (|| {
        let (key, _) = signed(state, request, "/api/v1/identity/restore")?;
        let db = state.db();
        let was_admin: Option<bool> = db
            .query_row("SELECT was_admin FROM deletions WHERE key = ?", [&key], |r| r.get(0))
            .optional()
            .map_err(|e| error(500, format!("database: {e}")))?;
        let Some(was_admin) = was_admin else { return Err(error(404, "there's nothing deleted to restore")) };
        let restored = (|| {
            let tx = db.unchecked_transaction()?;
            let scripts = tx.execute(
                "UPDATE scripts SET hidden = 0, hidden_reason = NULL WHERE author_key = ? AND hidden = 2",
                [&key],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO encores (script_id, key, ip, created)
                 SELECT d.script_id, d.key, d.ip, d.created FROM deleted_encores d JOIN scripts s ON s.id = d.script_id
                 WHERE d.key = ?",
                [&key],
            )?;
            tx.execute("DELETE FROM deleted_encores WHERE key = ?", [&key])?;
            tx.execute("UPDATE scripts SET encores = (SELECT COUNT(*) FROM encores WHERE script_id = scripts.id)", [])?;
            if was_admin {
                tx.execute("INSERT OR IGNORE INTO admins (key, added) VALUES (?, ?)", params![key, now()])?;
            }
            tx.execute("DELETE FROM deletions WHERE key = ?", [&key])?;
            tx.commit()?;
            Ok::<_, rusqlite::Error>(scripts)
        })()
        .map_err(|e| error(500, format!("database: {e}")))?;
        println!("#{}: restored", key_id(&key));
        Ok(restored)
    })();
    match result {
        Ok(scripts) => json(200, &serde_json::json!({ "restored": scripts })),
        Err(reply) => reply,
    }
}

/// Delete for good what's waited its 30 days (daily, and at startup).
pub(super) fn purge(state: &State) {
    purge_as_of(state, now());
}

/// `purge`, as if it were `time` (tests can't wait 30 days).
pub(super) fn purge_as_of(state: &State, time: i64) {
    let db = state.db();
    let due: Vec<String> = db
        .prepare("SELECT key FROM deletions WHERE purge_after < ?")
        .and_then(|mut s| s.query_map([time], |r| r.get(0))?.collect())
        .unwrap_or_default();
    for key in due {
        let scripts: Vec<String> = db
            .prepare("SELECT id FROM scripts WHERE author_key = ? AND hidden = 2")
            .and_then(|mut s| s.query_map([&key], |r| r.get(0))?.collect())
            .unwrap_or_default();
        let result = (|| {
            let tx = db.unchecked_transaction()?;
            tx.execute("DELETE FROM scripts WHERE author_key = ? AND hidden = 2", [&key])?;
            tx.execute("DELETE FROM deleted_encores WHERE key = ?", [&key])?;
            tx.execute("UPDATE reports SET reporter_key = '' WHERE reporter_key = ?", [&key])?;
            tx.execute("DELETE FROM identities WHERE key = ?", [&key])?;
            tx.execute("DELETE FROM codes WHERE key = ?", [&key])?;
            tx.execute("DELETE FROM recoveries WHERE key = ?", [&key])?;
            tx.execute("DELETE FROM deletions WHERE key = ?", [&key])?;
            tx.commit()
        })();
        match result {
            Ok(()) => {
                for id in &scripts {
                    remove_previews(state, id);
                }
                println!("#{}: deleted for good ({} scripts)", key_id(&key), scripts.len());
            }
            Err(e) => println!("couldn't purge #{}: {e}", key_id(&key)),
        }
    }
}
