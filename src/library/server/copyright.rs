//! Copyright notices (docs/SERVER.md), as the US DMCA has them.
//!
//! A notice (`POST /api/v1/notices`, signed) takes a script down at once
//! (`hidden = 3`), unless a dispute of an earlier notice was upheld (it's
//! locked): then it waits for the moderators. Its uploader is told, and
//! can dispute it (a counter-notice, sent on to the claimant) or let it
//! stand; with neither in 14 days, it stands. Moderators uphold a dispute
//! (the script comes back, locked), reject it (it stands), or find the
//! notice bogus (it comes back). A takedown that stands puts the script's
//! fingerprints on a blocklist (the same code can't simply come back), and
//! a key with three that stood is banned (the repeat-infringer policy).

use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use tiny_http::Request;

use super::{error, json, notifications, now, read_body, signer, Reply, State};
use crate::library::fingerprint::{Fingerprint, COPY};
use crate::library::{key_id, CopyrightNotice, CounterNotice, NoticeView};

pub(super) const SCHEMA_V11: &str = "
BEGIN;
CREATE TABLE notices (
    id INTEGER PRIMARY KEY,
    item TEXT NOT NULL,
    item_name TEXT NOT NULL,
    author_key TEXT,                -- the uploader's, when it was filed
    claimant_key TEXT NOT NULL,
    notice TEXT NOT NULL,           -- the CopyrightNotice, JSON
    counter TEXT,                   -- the CounterNotice, JSON
    status TEXT NOT NULL,
    ip TEXT,
    created INTEGER NOT NULL,
    deadline INTEGER NOT NULL,
    decided INTEGER,
    decision_note TEXT
);
CREATE INDEX notices_item ON notices(item);
CREATE TABLE blocked (fingerprint TEXT NOT NULL, notice INTEGER NOT NULL, created INTEGER NOT NULL);
CREATE TABLE takedowns (key TEXT NOT NULL, notice INTEGER NOT NULL, created INTEGER NOT NULL);
ALTER TABLE scripts ADD COLUMN copyright_locked INTEGER NOT NULL DEFAULT 0;
PRAGMA user_version = 11;
COMMIT;
";

/// How long an uploader has to dispute a notice before it stands.
const DISPUTE_SECONDS: i64 = 14 * 24 * 60 * 60;
/// Notices one address may file in a day.
const NOTICES_PER_DAY: usize = 5;
/// Takedowns that stood before a key is banned.
const REPEAT_INFRINGER: i64 = 3;

fn db_error(e: rusqlite::Error) -> Reply {
    error(500, format!("database: {e}"))
}

/// A notice, as stored: (its fields, the counter-notice, author, claimant).
struct Stored {
    view: NoticeView,
    author_key: Option<String>,
    claimant_key: String,
}

fn stored(db: &Connection, id: i64) -> rusqlite::Result<Option<Stored>> {
    db.query_row(
        "SELECT id, item, item_name, status, created, deadline, notice, counter, decision_note, author_key, claimant_key
         FROM notices WHERE id = ?",
        [id],
        row,
    )
    .optional()
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Stored> {
    let author_key: Option<String> = r.get(9)?;
    let claimant_key: String = r.get(10)?;
    Ok(Stored {
        view: NoticeView {
            id: r.get(0)?,
            item: r.get(1)?,
            item_name: r.get(2)?,
            status: r.get(3)?,
            created: r.get(4)?,
            deadline: r.get(5)?,
            notice: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
            counter: r.get::<_, Option<String>>(7)?.and_then(|c| serde_json::from_str(&c).ok()),
            decision_note: r.get(8)?,
            claimant_id: Some(key_id(&claimant_key)),
            uploader_id: author_key.as_deref().map(key_id),
        },
        author_key,
        claimant_key,
    })
}

/// The fields a notice needs, checked.
fn check_notice(notice: &CopyrightNotice) -> Result<(), String> {
    let required = [
        (&notice.name, "your full name"),
        (&notice.on_behalf_of, "whose the rights are"),
        (&notice.address, "your postal address"),
        (&notice.email, "your email address"),
        (&notice.phone, "your phone number"),
        (&notice.work, "the copyrighted work"),
        (&notice.signature, "your signature (your full name, typed)"),
    ];
    for (value, what) in required {
        if value.trim().is_empty() {
            return Err(format!("a notice needs {what}"));
        }
        if value.chars().count() > 2000 {
            return Err(format!("{what}: that's too long"));
        }
    }
    if !notice.good_faith || !notice.accurate {
        return Err("a notice needs both statements".into());
    }
    if !crate::library::rotation::is_email(&notice.email) {
        return Err("that email address doesn't look right".into());
    }
    Ok(())
}

fn check_counter(counter: &CounterNotice) -> Result<(), String> {
    let required = [
        (&counter.name, "your full name"),
        (&counter.address, "your postal address"),
        (&counter.email, "your email address"),
        (&counter.signature, "your signature (your full name, typed)"),
    ];
    for (value, what) in required {
        if value.trim().is_empty() {
            return Err(format!("a dispute needs {what}"));
        }
        if value.chars().count() > 2000 {
            return Err(format!("{what}: that's too long"));
        }
    }
    if counter.explanation.chars().count() > 4000 {
        return Err("the explanation can be up to 4000 characters".into());
    }
    if !counter.mistake || !counter.consent {
        return Err("a dispute needs both statements".into());
    }
    Ok(())
}

/// Take `item` down under notice `id` (hidden = 3), telling its uploader.
fn take_down(state: &State, db: &Connection, id: i64, item: &str, name: &str, notice: &CopyrightNotice) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE scripts SET hidden = 3, hidden_reason = ? WHERE id = ? AND hidden IN (0, 1)",
        params![format!("copyright notice #{id}"), item],
    )?;
    db.execute(
        "UPDATE notices SET status = 'taken_down', deadline = ? WHERE id = ?",
        params![now() + DISPUTE_SECONDS, id],
    )?;
    notifications::author_notify_any(
        state,
        db,
        item,
        "copyright_notice",
        serde_json::json!({
            "notice": id, "script": item, "name": name,
            "claimant": notice.name, "on_behalf_of": notice.on_behalf_of, "work": notice.work,
        }),
        true,
    );
    Ok(())
}

/// The takedown stands: blocklisted, counted against the uploader (and
/// their key banned at three), and they're told.
fn stand(state: &State, db: &Connection, notice: &Stored, how: &str) -> rusqlite::Result<()> {
    let id = notice.view.id;
    db.execute("UPDATE notices SET status = 'stood', decided = ? WHERE id = ?", params![now(), id])?;
    let fingerprints: Vec<String> = db
        .prepare("SELECT fingerprint FROM versions WHERE script_id = ? AND fingerprint IS NOT NULL")?
        .query_map([&notice.view.item], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for fingerprint in fingerprints {
        db.execute("INSERT INTO blocked (fingerprint, notice, created) VALUES (?, ?, ?)", params![fingerprint, id, now()])?;
    }
    if let Some(author) = &notice.author_key {
        db.execute("INSERT INTO takedowns (key, notice, created) VALUES (?, ?, ?)", params![author, id, now()])?;
        let stood: i64 = db.query_row("SELECT COUNT(*) FROM takedowns WHERE key = ?", [author], |r| r.get(0))?;
        if stood >= REPEAT_INFRINGER {
            db.execute(
                "INSERT OR REPLACE INTO bans (target, until, reason, created) VALUES (?, NULL, ?, ?)",
                params![author, format!("repeat infringer ({stood} copyright takedowns)"), now()],
            )?;
            println!("banned #{} (repeat infringer)", key_id(author));
        }
        let fields = serde_json::json!({ "notice": id, "script": notice.view.item, "name": notice.view.item_name, "how": how });
        notifications::notify(state, db, author, "takedown_stood", fields, false);
        mark_handled(db, author, id, how);
    }
    println!("copyright notice #{id}: the takedown stands ({how})");
    Ok(())
}

/// The uploader's notification of notice `id` is done with: `resolution`
/// says how.
fn mark_handled(db: &Connection, key: &str, id: i64, resolution: &str) {
    notifications::resolve(db, key, "copyright_notice", "notice", id, resolution);
}

/// Whether `fingerprint` is (nearly) a script taken down for good.
pub(super) fn blocked(db: &Connection, fingerprint: &Fingerprint) -> rusqlite::Result<bool> {
    let stored: Vec<String> = db.prepare("SELECT fingerprint FROM blocked")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(stored.iter().filter_map(|f| Fingerprint::from_hex(f)).any(|f| f.similarity(fingerprint) >= COPY))
}

/// `POST /api/v1/notices`, signed: file a notice.
pub(super) fn file(state: &State, request: &mut Request, ip: &str) -> Reply {
    let result = (|| {
        let body = read_body(request, 64 * 1024)?;
        let key = signer(state, request, "POST", "/api/v1/notices", &body)?
            .ok_or_else(|| error(401, "a notice needs your signature"))?;
        super::moderation::refuse_banned(state, Some(&key), ip)?;
        let notice: CopyrightNotice = serde_json::from_slice(&body).map_err(|e| error(400, format!("not a notice: {e}")))?;
        check_notice(&notice).map_err(|e| error(400, e))?;
        let db = state.db();
        let found: Option<(String, Option<String>, bool, i64)> = db
            .query_row(
                "SELECT name, author_key, copyright_locked, hidden FROM scripts WHERE id = ?",
                [&notice.item],
                |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0, r.get(3)?)),
            )
            .optional()
            .map_err(db_error)?;
        let Some((name, author, locked, hidden)) = found.filter(|f| f.3 != 2) else {
            return Err(error(404, "no such script"));
        };
        if author.as_deref() == Some(key.as_str()) {
            return Err(error(400, "that's your own upload: delete it instead"));
        }
        if hidden == 3 {
            return Err(error(409, "it's already down under a copyright notice"));
        }
        // (Counted once it's a notice that would be filed.)
        super::moderation::limit(state, "notices", ip, NOTICES_PER_DAY, Duration::from_secs(24 * 3600), "notices")?;
        db.execute(
            "INSERT INTO notices (item, item_name, author_key, claimant_key, notice, status, ip, created, deadline)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                notice.item,
                name,
                author,
                key,
                serde_json::to_string(&notice).unwrap_or_default(),
                if locked { "queued" } else { "taken_down" },
                ip,
                now(),
                now() + DISPUTE_SECONDS
            ],
        )
        .map_err(db_error)?;
        let id = db.last_insert_rowid();
        if !locked {
            take_down(state, &db, id, &notice.item, &name, &notice).map_err(db_error)?;
        }
        let fields = serde_json::json!({ "notice": id, "script": notice.item, "name": name, "queued": locked });
        notifications::notify_admins(state, &db, "new_notice", fields);
        println!("copyright notice #{id} on {} \"{name}\" ({})", notice.item, if locked { "queued: locked" } else { "taken down" });
        Ok(serde_json::json!({ "notice": id, "status": if locked { "queued" } else { "taken_down" } }))
    })();
    match result {
        Ok(reply) => json(201, &reply),
        Err(reply) => reply,
    }
}

/// The notice's uploader or claimant, from a signed request to `path`.
fn party(state: &State, request: &mut Request, path: &str) -> Result<(String, Vec<u8>), Reply> {
    let body = read_body(request, 64 * 1024)?;
    let key = signer(state, request, "POST", path, &body)?.ok_or_else(|| error(401, "that needs your signature"))?;
    Ok((key, body))
}

/// `POST /api/v1/notices/{id}`, signed by its uploader or claimant: the
/// notice, as they may see it.
pub(super) fn view(state: &State, request: &mut Request, id: i64) -> Reply {
    let result = (|| {
        let (key, _) = party(state, request, &format!("/api/v1/notices/{id}"))?;
        let db = state.db();
        let notice = stored(&db, id).map_err(db_error)?.ok_or_else(|| error(404, "no such notice"))?;
        let mut view = notice.view;
        if notice.author_key.as_deref() == Some(key.as_str()) {
            // (The uploader doesn't get the claimant's contact details.)
            view.notice.address.clear();
            view.notice.email.clear();
            view.notice.phone.clear();
        } else if notice.claimant_key != key {
            return Err(error(403, "that's not a notice about you or from you"));
        }
        view.claimant_id = None;
        view.uploader_id = None;
        Ok(view)
    })();
    match result {
        Ok(view) => json(200, &view),
        Err(reply) => reply,
    }
}

/// `POST /api/v1/notices/{id}/dispute`, signed by the uploader: a
/// counter-notice.
pub(super) fn dispute(state: &State, request: &mut Request, id: i64) -> Reply {
    let result = (|| {
        let (key, body) = party(state, request, &format!("/api/v1/notices/{id}/dispute"))?;
        let counter: CounterNotice = serde_json::from_slice(&body).map_err(|e| error(400, format!("not a dispute: {e}")))?;
        check_counter(&counter).map_err(|e| error(400, e))?;
        let db = state.db();
        let notice = stored(&db, id).map_err(db_error)?.ok_or_else(|| error(404, "no such notice"))?;
        if notice.author_key.as_deref() != Some(key.as_str()) {
            return Err(error(403, "only its uploader can dispute it"));
        }
        if notice.view.status != "taken_down" {
            return Err(error(409, format!("it can't be disputed now (it's {})", notice.view.status.replace('_', " "))));
        }
        db.execute(
            "UPDATE notices SET status = 'disputed', counter = ? WHERE id = ?",
            params![serde_json::to_string(&counter).unwrap_or_default(), id],
        )
        .map_err(db_error)?;
        mark_handled(&db, &key, id, "disputed");
        let item = (&notice.view.item, &notice.view.item_name);
        // (The counter-notice goes to the claimant, as the DMCA has it.)
        let fields = serde_json::json!({
            "notice": id, "script": item.0, "name": item.1,
            "uploader": counter.name, "address": counter.address, "email": counter.email,
        });
        notifications::notify(state, &db, &notice.claimant_key, "notice_disputed", fields, false);
        notifications::notify_admins(state, &db, "new_dispute", serde_json::json!({ "notice": id, "script": item.0, "name": item.1 }));
        println!("copyright notice #{id}: disputed");
        Ok(())
    })();
    match result {
        Ok(()) => json(200, &serde_json::json!({ "status": "disputed" })),
        Err(reply) => reply,
    }
}

/// `POST /api/v1/notices/{id}/accept`, signed by the uploader: let it stand.
pub(super) fn accept(state: &State, request: &mut Request, id: i64) -> Reply {
    let result = (|| {
        let (key, _) = party(state, request, &format!("/api/v1/notices/{id}/accept"))?;
        let db = state.db();
        let notice = stored(&db, id).map_err(db_error)?.ok_or_else(|| error(404, "no such notice"))?;
        if notice.author_key.as_deref() != Some(key.as_str()) {
            return Err(error(403, "only its uploader can let it stand"));
        }
        if notice.view.status != "taken_down" {
            return Err(error(409, format!("it's {} already", notice.view.status.replace('_', " "))));
        }
        stand(state, &db, &notice, "let stand").map_err(db_error)
    })();
    match result {
        Ok(()) => json(200, &serde_json::json!({ "status": "stood" })),
        Err(reply) => reply,
    }
}

/// Takedowns nobody disputed in time stand (daily, and at startup).
pub(super) fn expire(state: &State) {
    let db = state.db();
    let due: Vec<Stored> = db
        .prepare(
            "SELECT id, item, item_name, status, created, deadline, notice, counter, decision_note, author_key, claimant_key
             FROM notices WHERE status = 'taken_down' AND deadline < ?",
        )
        .and_then(|mut s| s.query_map([now()], row)?.collect())
        .unwrap_or_default();
    for notice in due {
        if let Err(e) = stand(state, &db, &notice, "not disputed in 14 days") {
            println!("couldn't let notice #{} stand: {e}", notice.view.id);
        }
    }
}

/// Moderators: the notices (open ones, or all), with everything.
pub(super) fn list(db: &Connection, all: bool) -> rusqlite::Result<Vec<NoticeView>> {
    let mut statement = db.prepare(
        "SELECT id, item, item_name, status, created, deadline, notice, counter, decision_note, author_key, claimant_key
         FROM notices WHERE ?1 OR status IN ('taken_down', 'queued', 'disputed') ORDER BY id DESC LIMIT 500",
    )?;
    statement.query_map([all], row)?.map(|r| r.map(|s| s.view)).collect()
}

/// Moderators: decide a notice.
pub(super) fn decide(state: &State, db: &Connection, id: i64, decision: &str, note: &str) -> Result<String, (u16, String)> {
    let db_err = |e: rusqlite::Error| (500, format!("database: {e}"));
    let notice = stored(db, id).map_err(db_err)?.ok_or((404, "no such notice".to_string()))?;
    let (status, item, name) = (notice.view.status.as_str(), notice.view.item.clone(), notice.view.item_name.clone());
    let decided = |status: &str| {
        db.execute(
            "UPDATE notices SET status = ?, decided = ?, decision_note = ? WHERE id = ?",
            params![status, now(), note, id],
        )
    };
    let fields = serde_json::json!({ "notice": id, "script": item, "name": name, "note": note });
    match (decision, status) {
        ("uphold_dispute", "disputed") => {
            decided("upheld").map_err(db_err)?;
            db.execute("UPDATE scripts SET hidden = 0, hidden_reason = NULL, copyright_locked = 1 WHERE id = ? AND hidden = 3", [&item])
                .map_err(db_err)?;
            if let Some(author) = &notice.author_key {
                notifications::notify(state, db, author, "dispute_upheld", fields.clone(), false);
            }
            notifications::notify(state, db, &notice.claimant_key, "notice_rejected", fields, false);
            Ok("Dispute upheld: it's back up, locked against further notices.".into())
        }
        ("reject_dispute", "disputed") => {
            decided("stood").map_err(db_err)?;
            let mut notice = notice;
            notice.view.status = "stood".into();
            stand(state, db, &notice, "the dispute was rejected").map_err(db_err)?;
            Ok("Dispute rejected: the takedown stands.".into())
        }
        ("bogus", "taken_down" | "queued" | "disputed") => {
            decided("bogus").map_err(db_err)?;
            db.execute("UPDATE scripts SET hidden = 0, hidden_reason = NULL WHERE id = ? AND hidden = 3", [&item]).map_err(db_err)?;
            if let Some(author) = &notice.author_key {
                if status != "queued" {
                    notifications::notify(state, db, author, "notice_withdrawn", fields.clone(), false);
                }
                mark_handled(db, author, id, "found bogus");
            }
            notifications::notify(state, db, &notice.claimant_key, "notice_rejected", fields, false);
            Ok("Notice found bogus: the script's up again.".into())
        }
        ("take_down", "queued") => {
            take_down(state, db, id, &item, &name, &notice.view.notice).map_err(db_err)?;
            Ok("Taken down: its uploader can dispute it.".into())
        }
        (_, status) => Err((400, format!("can't {} a notice that's {}", decision.replace('_', " "), status.replace('_', " ")))),
    }
}
