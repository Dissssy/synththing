//! Encores, reports and moderation (docs/SERVER.md).
//!
//! An encore is the only vote: one per key per script, signed, taken back
//! with a DELETE; nobody can encore their own. A report (signed) says what's
//! wrong with a version of a script and can point at lines of its code and
//! sprites in it; one open report per key per script. Both are limited per
//! address, and refused from banned keys and addresses.
//!
//! Admins are keys listed in the database, and the server's own key
//! (which `synththing admin` signs with, reading it from the data folder):
//! one endpoint, `POST /api/v1/admin`, takes an `AdminAction`. Addresses
//! are kept 30 days (bans keep theirs until they end). A key ban can take
//! its addresses along: those it was seen on in the last 30 days are
//! banned for 30 days, and so is any new one it comes back from.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OptionalExtension};
use tiny_http::Request;

use super::{error, json, now, read_body, remove_previews, signer, Reply, State};
use crate::library::{
    key_id, AdminAction, AdminScriptInfo, ApiError, Ban, EncoreState, Report, ReportRequest, MAX_REPORT_DETAILS,
    MAX_REPORT_MARKS, REPORT_REASONS,
};

pub(super) const SCHEMA_V5: &str = "
BEGIN;
CREATE TABLE encores (
    script_id TEXT NOT NULL REFERENCES scripts(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    ip TEXT,
    created INTEGER NOT NULL,
    PRIMARY KEY (script_id, key)
);
CREATE TABLE reports (
    id INTEGER PRIMARY KEY,
    script_id TEXT NOT NULL,        -- kept when the script goes
    script_name TEXT NOT NULL,
    version INTEGER NOT NULL,
    reporter_key TEXT NOT NULL,
    reason TEXT NOT NULL,
    details TEXT NOT NULL,
    lines TEXT NOT NULL,            -- JSON
    sprites TEXT NOT NULL,          -- JSON
    ip TEXT,
    created INTEGER NOT NULL,
    resolved INTEGER,
    resolution TEXT
);
CREATE INDEX reports_script ON reports(script_id);
CREATE TABLE admins (key TEXT PRIMARY KEY, added INTEGER NOT NULL);
CREATE TABLE bans (target TEXT PRIMARY KEY, until INTEGER, reason TEXT NOT NULL, created INTEGER NOT NULL);
ALTER TABLE scripts ADD COLUMN hidden_reason TEXT;
PRAGMA user_version = 5;
COMMIT;
";

/// Encores one address may give or take back in an hour, and reports it
/// may make in a day.
const ENCORES_PER_HOUR: usize = 60;
const REPORTS_PER_DAY: usize = 10;
/// How long addresses are kept, and how long a banned key's addresses are
/// banned for.
const KEEP_ADDRESSES: i64 = 30 * 24 * 60 * 60;

/// Schema 6: key bans that spread to the key's addresses.
pub(super) const SCHEMA_V6: &str = "
BEGIN;
ALTER TABLE bans ADD COLUMN spreads INTEGER NOT NULL DEFAULT 0;
PRAGMA user_version = 6;
COMMIT;
";

/// Ban `address` until `until` (unless it's banned longer already).
fn ban_address(db: &Connection, address: &str, until: i64, reason: &str) -> rusqlite::Result<usize> {
    db.execute(
        "INSERT INTO bans (target, until, reason, created) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(target) DO UPDATE SET
             until = CASE WHEN bans.until IS NULL THEN NULL ELSE MAX(bans.until, excluded.until) END,
             reason = CASE WHEN bans.until IS NULL OR bans.until >= excluded.until THEN bans.reason ELSE excluded.reason END",
        params![address, until, reason, now()],
    )
}

/// The addresses `key` was seen on in the last 30 days.
fn addresses_of(db: &Connection, key: &str) -> rusqlite::Result<Vec<String>> {
    let since = now() - KEEP_ADDRESSES;
    db.prepare(
        "SELECT v.uploader_ip FROM versions v JOIN scripts s ON s.id = v.script_id
             WHERE s.author_key = ?1 AND v.created >= ?2 AND v.uploader_ip IS NOT NULL
         UNION SELECT ip FROM encores WHERE key = ?1 AND created >= ?2 AND ip IS NOT NULL
         UNION SELECT ip FROM reports WHERE reporter_key = ?1 AND created >= ?2 AND ip IS NOT NULL",
    )?
    .query_map(params![key, since], |r| r.get(0))?
    .collect()
}

/// One more `bucket` request from `ip`, unless it's had `most` in `window`.
fn limit(state: &State, bucket: &str, ip: &str, most: usize, window: Duration, what: &str) -> Result<(), Reply> {
    let mut limits = state.limits.lock().unwrap_or_else(|p| p.into_inner());
    let recent = limits.entry((bucket.to_string(), ip.to_string())).or_default();
    recent.retain(|t| t.elapsed() < window);
    if recent.len() >= most {
        let wait = recent.first().map(|t| window.saturating_sub(t.elapsed()).as_secs()).unwrap_or(0);
        return Err(json(429, &ApiError { error: format!("that's all the {what} for now from here"), retry_after: Some(wait) }));
    }
    recent.push(Instant::now());
    Ok(())
}

/// The ban on `target` in force, if there is one: (until, reason,
/// whether it spreads to the addresses it's seen on).
fn ban_on(db: &Connection, target: &str) -> Option<(Option<i64>, String, bool)> {
    db.query_row(
        "SELECT until, reason, spreads FROM bans WHERE target = ? AND (until IS NULL OR until > ?)",
        params![target, now()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
    )
    .optional()
    .ok()
    .flatten()
}

/// Refused if the key or the address is banned. A banned key whose ban
/// spreads bans the address it came from, too, for 30 days.
pub(super) fn refuse_banned(state: &State, key: Option<&str>, ip: &str) -> Result<(), Reply> {
    let db = state.db();
    if let Some(key) = key
        && let Some((_, reason, true)) = ban_on(&db, key)
        && !ip.is_empty()
        && ban_address(&db, ip, now() + KEEP_ADDRESSES, &format!("seen with the banned key #{}: {reason}", key_id(key))).is_ok()
    {
        println!("banned {ip} for 30 days (seen with the banned key #{})", key_id(key));
    }
    for target in key.into_iter().chain([ip]) {
        if let Some((until, reason, _)) = ban_on(&db, target) {
            let how_long = until
                .map(|u| format!(" for another {}", crate::song_info::format_length((u - now()).max(0) as f64)))
                .unwrap_or_default();
            return Err(error(403, format!("you're banned from this server{how_long}: {reason}")));
        }
    }
    Ok(())
}

/// `POST` (or `DELETE`) `/api/v1/scripts/{id}/encore`, signed.
pub(super) fn encore(state: &State, request: &mut Request, id: &str, give: bool, ip: &str) -> Reply {
    let path = format!("/api/v1/scripts/{id}/encore");
    let key = match signer(state, request, if give { "POST" } else { "DELETE" }, &path, &[]) {
        Ok(Some(key)) => key,
        Ok(None) => return error(401, "an encore needs your signature"),
        Err(reply) => return reply,
    };
    if let Err(reply) = refuse_banned(state, Some(&key), ip)
        .and_then(|()| limit(state, "encores", ip, ENCORES_PER_HOUR, Duration::from_secs(3600), "encores"))
    {
        return reply;
    }
    let db = state.db();
    let author: rusqlite::Result<Option<Option<String>>> =
        db.query_row("SELECT author_key FROM scripts WHERE id = ? AND hidden = 0", [id], |r| r.get(0)).optional();
    match author {
        Ok(None) => return error(404, "no such script"),
        Ok(Some(author)) if author.as_deref() == Some(key.as_str()) => {
            return error(400, "you can't give your own script an encore");
        }
        Ok(Some(_)) => {}
        Err(e) => return error(500, format!("database: {e}")),
    }
    let changed = if give {
        db.execute("INSERT OR IGNORE INTO encores (script_id, key, ip, created) VALUES (?, ?, ?, ?)", params![id, key, ip, now()])
    } else {
        db.execute("DELETE FROM encores WHERE script_id = ? AND key = ?", params![id, key])
    }
    .and_then(|_| {
        db.execute("UPDATE scripts SET encores = (SELECT COUNT(*) FROM encores WHERE script_id = ?1) WHERE id = ?1", [id])
    })
    .and_then(|_| db.query_row("SELECT encores FROM scripts WHERE id = ?", [id], |r| r.get::<_, i64>(0)));
    match changed {
        Ok(encores) => json(200, &EncoreState { encores: encores.max(0) as u64, encored: give }),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// Whether `key` has given `id` an encore (for details' `?viewer=`).
pub(super) fn has_encored(db: &Connection, id: &str, key: &str) -> bool {
    db.query_row("SELECT 1 FROM encores WHERE script_id = ? AND key = ?", params![id, key], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

/// `POST /api/v1/scripts/{id}/report`, signed.
pub(super) fn report(state: &State, request: &mut Request, id: &str, ip: &str) -> Reply {
    let body = match read_body(request, 16 * 1024) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let path = format!("/api/v1/scripts/{id}/report");
    let key = match signer(state, request, "POST", &path, &body) {
        Ok(Some(key)) => key,
        Ok(None) => return error(401, "a report needs your signature"),
        Err(reply) => return reply,
    };
    if let Err(reply) = refuse_banned(state, Some(&key), ip)
        .and_then(|()| limit(state, "reports", ip, REPORTS_PER_DAY, Duration::from_secs(24 * 3600), "reports"))
    {
        return reply;
    }
    let report: ReportRequest = match serde_json::from_slice(&body) {
        Ok(report) => report,
        Err(e) => return error(400, format!("not a report: {e}")),
    };
    if !REPORT_REASONS.iter().any(|(r, _)| *r == report.reason) {
        return error(400, format!("\"{}\" isn't a reason to report something", report.reason));
    }
    if report.details.chars().count() > MAX_REPORT_DETAILS {
        return error(400, format!("the details can be up to {MAX_REPORT_DETAILS} characters"));
    }
    if report.lines.len() + report.sprites.len() > MAX_REPORT_MARKS {
        return error(400, format!("a report can point at up to {MAX_REPORT_MARKS} lines and sprites"));
    }
    let db = state.db();
    let name: rusqlite::Result<Option<String>> = db
        .query_row(
            "SELECT s.name FROM scripts s JOIN versions v ON v.script_id = s.id WHERE s.id = ? AND v.version = ? AND s.hidden = 0",
            params![id, report.version],
            |r| r.get(0),
        )
        .optional();
    let name = match name {
        Ok(Some(name)) => name,
        Ok(None) => return error(404, "no such script or version"),
        Err(e) => return error(500, format!("database: {e}")),
    };
    let open = db
        .query_row("SELECT 1 FROM reports WHERE script_id = ? AND reporter_key = ? AND resolved IS NULL", params![id, key], |_| {
            Ok(())
        })
        .optional();
    if matches!(open, Ok(Some(()))) {
        return error(409, "you've reported it already: the moderators will look at it");
    }
    let stored = db.execute(
        "INSERT INTO reports (script_id, script_name, version, reporter_key, reason, details, lines, sprites, ip, created)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            name,
            report.version,
            key,
            report.reason,
            report.details.trim(),
            serde_json::to_string(&report.lines).unwrap_or_default(),
            serde_json::to_string(&report.sprites).unwrap_or_default(),
            ip,
            now()
        ],
    );
    match stored {
        Ok(_) => {
            let number = db.last_insert_rowid();
            println!("report #{number} of {id} \"{name}\" ({}) by #{}", report.reason, key_id(&key));
            json(201, &serde_json::json!({ "report": number }))
        }
        Err(e) => error(500, format!("database: {e}")),
    }
}

fn is_admin(state: &State, key: &str) -> bool {
    key == state.key_hex
        || state.db().query_row("SELECT 1 FROM admins WHERE key = ?", [key], |_| Ok(())).optional().ok().flatten().is_some()
}

/// `POST /api/v1/admin`, signed by an admin.
pub(super) fn admin(state: &State, request: &mut Request) -> Reply {
    let body = match read_body(request, 64 * 1024) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let key = match signer(state, request, "POST", "/api/v1/admin", &body) {
        Ok(Some(key)) => key,
        Ok(None) => return error(401, "that needs an admin's signature"),
        Err(reply) => return reply,
    };
    if !is_admin(state, &key) {
        return error(403, "only this server's admins can do that");
    }
    let action: AdminAction = match serde_json::from_slice(&body) {
        Ok(action) => action,
        Err(e) => return error(400, format!("not an admin action: {e}")),
    };
    let who = if key == state.key_hex { "the server".to_string() } else { format!("#{}", key_id(&key)) };
    match act(state, &action) {
        Ok(reply) => {
            if !matches!(
                action,
                AdminAction::Whoami
                    | AdminAction::Reports { .. }
                    | AdminAction::Info { .. }
                    | AdminAction::Source { .. }
                    | AdminAction::Bans
                    | AdminAction::Admins
            ) {
                println!("admin {who}: {}", serde_json::to_string(&action).unwrap_or_default());
            }
            json(200, &reply)
        }
        Err((status, message)) => error(status, message),
    }
}

type Acted = Result<serde_json::Value, (u16, String)>;

fn db_error(e: rusqlite::Error) -> (u16, String) {
    (500, format!("database: {e}"))
}

fn act(state: &State, action: &AdminAction) -> Acted {
    let value = |v: serde_json::Result<serde_json::Value>| v.map_err(|e| (500, e.to_string()));
    let db = state.db();
    match action {
        AdminAction::Whoami => Ok(serde_json::json!({ "admin": true })),
        AdminAction::Reports { all } => value(serde_json::to_value(reports(&db, None, *all).map_err(db_error)?)),
        AdminAction::Info { script } => {
            let summary = db
                .query_row(
                    &format!("SELECT {}, hidden, hidden_reason FROM scripts WHERE id = ?", super::SUMMARY_COLUMNS),
                    [script],
                    |r| Ok((super::summary_from_row(r)?, r.get::<_, i64>(12)? != 0, r.get::<_, Option<String>>(13)?)),
                )
                .optional()
                .map_err(db_error)?
                .ok_or((404, "no such script".to_string()))?;
            let author_key: Option<String> =
                db.query_row("SELECT author_key FROM scripts WHERE id = ?", [script], |r| r.get(0)).map_err(db_error)?;
            let uploads = db
                .prepare("SELECT version, uploader_ip, created FROM versions WHERE script_id = ? ORDER BY version")
                .and_then(|mut s| s.query_map([script], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect())
                .map_err(db_error)?;
            let (summary, hidden, hidden_reason) = summary;
            let info = AdminScriptInfo {
                summary,
                hidden,
                hidden_reason,
                author_key,
                uploads,
                reports: reports(&db, Some(script), true).map_err(db_error)?,
            };
            value(serde_json::to_value(info))
        }
        AdminAction::Source { script, version } => {
            let found: Option<(u32, String)> = db
                .query_row(
                    "SELECT version, source FROM versions WHERE script_id = ?1
                     AND version = COALESCE(?2, (SELECT latest FROM scripts WHERE id = ?1))",
                    params![script, version],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(db_error)?;
            let (version, source) = found.ok_or((404, "no such script or version".to_string()))?;
            Ok(serde_json::json!({ "version": version, "source": source }))
        }
        AdminAction::Hide { script, reason } => {
            let n = db
                .execute("UPDATE scripts SET hidden = 1, hidden_reason = ? WHERE id = ?", params![reason, script])
                .map_err(db_error)?;
            changed(n, "hidden")
        }
        AdminAction::Unhide { script } => {
            let n = db
                .execute("UPDATE scripts SET hidden = 0, hidden_reason = NULL WHERE id = ?", [script])
                .map_err(db_error)?;
            changed(n, "unhidden")
        }
        AdminAction::Delete { script } => {
            let n = db.execute("DELETE FROM scripts WHERE id = ?", [script]).map_err(db_error)?;
            drop(db);
            if n > 0 {
                remove_previews(state, script);
            }
            changed(n, "deleted")
        }
        AdminAction::Ban { target, hours, reason, hide_scripts, ban_addresses } => {
            let target = resolve_target(&db, target)?;
            if target == state.key_hex {
                return Err((400, "that's this server's own key".into()));
            }
            let is_key = target.len() == 64;
            let until = hours.map(|h| now() + (h as i64) * 3600);
            let spreads = *ban_addresses && is_key;
            db.execute(
                "INSERT OR REPLACE INTO bans (target, until, reason, created, spreads) VALUES (?, ?, ?, ?, ?)",
                params![target, until, reason, now(), spreads],
            )
            .map_err(db_error)?;
            let mut addresses = 0;
            if spreads {
                let why = format!("used by the banned key #{}: {reason}", key_id(&target));
                for address in addresses_of(&db, &target).map_err(db_error)? {
                    addresses += ban_address(&db, &address, now() + KEEP_ADDRESSES, &why).map_err(db_error)?;
                }
            }
            let hidden = if *hide_scripts {
                db.execute(
                    "UPDATE scripts SET hidden = 1, hidden_reason = ? WHERE author_key = ? AND hidden = 0",
                    params![format!("banned: {reason}"), target],
                )
                .map_err(db_error)?
            } else {
                0
            };
            Ok(serde_json::json!({ "banned": target, "scripts_hidden": hidden, "addresses_banned": addresses }))
        }
        AdminAction::Unban { target } => {
            let target = resolve_target(&db, target)?;
            let n = db.execute("DELETE FROM bans WHERE target = ?", [&target]).map_err(db_error)?;
            changed(n, "unbanned")
        }
        AdminAction::Bans => {
            let bans: Vec<Ban> = db
                .prepare("SELECT target, until, reason, created FROM bans WHERE until IS NULL OR until > ? ORDER BY created DESC")
                .and_then(|mut s| {
                    s.query_map([now()], |r| Ok(Ban { target: r.get(0)?, until: r.get(1)?, reason: r.get(2)?, created: r.get(3)? }))?
                        .collect()
                })
                .map_err(db_error)?;
            value(serde_json::to_value(bans))
        }
        AdminAction::AddAdmin { key } => {
            let key = resolve_key(&db, key)?;
            db.execute("INSERT OR IGNORE INTO admins (key, added) VALUES (?, ?)", params![key, now()]).map_err(db_error)?;
            Ok(serde_json::json!({ "admin": key_id(&key) }))
        }
        AdminAction::RemoveAdmin { key } => {
            let key = resolve_key(&db, key)?;
            let n = db.execute("DELETE FROM admins WHERE key = ?", [&key]).map_err(db_error)?;
            changed(n, "removed")
        }
        AdminAction::Admins => {
            let keys: Vec<String> = db
                .prepare("SELECT key FROM admins ORDER BY added")
                .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
                .map_err(db_error)?;
            Ok(serde_json::json!(keys.iter().map(|k| [k.clone(), key_id(k)]).collect::<Vec<_>>()))
        }
        AdminAction::Resolve { report, note } => {
            let n = db
                .execute(
                    "UPDATE reports SET resolved = ?, resolution = ? WHERE id = ? AND resolved IS NULL",
                    params![now(), note, report],
                )
                .map_err(db_error)?;
            changed(n, "resolved")
        }
    }
}

fn changed(n: usize, what: &str) -> Acted {
    if n == 0 {
        return Err((404, "nothing like that here (or it's that way already)".into()));
    }
    Ok(serde_json::json!({ "done": what }))
}

/// Reports, newest first: open ones (or all), of one script (or any).
fn reports(db: &Connection, script: Option<&str>, all: bool) -> rusqlite::Result<Vec<Report>> {
    let mut statement = db.prepare(
        "SELECT r.id, r.script_id, r.script_name, r.version, r.reporter_key, r.reason, r.details, r.lines, r.sprites,
                r.created, r.resolved, r.resolution, s.id IS NOT NULL, COALESCE(s.hidden, 0)
         FROM reports r LEFT JOIN scripts s ON s.id = r.script_id
         WHERE (?1 IS NULL OR r.script_id = ?1) AND (?2 OR r.resolved IS NULL)
         ORDER BY r.id DESC LIMIT 500",
    )?;
    statement
        .query_map(params![script, all], |r| {
            let key: String = r.get(4)?;
            Ok(Report {
                id: r.get(0)?,
                script_id: r.get(1)?,
                script_name: r.get(2)?,
                request: ReportRequest {
                    version: r.get(3)?,
                    reason: r.get(5)?,
                    details: r.get(6)?,
                    lines: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                    sprites: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
                },
                reporter_id: key_id(&key),
                created: r.get(9)?,
                resolved: r.get(10)?,
                resolution: r.get(11)?,
                exists: r.get(12)?,
                hidden: r.get::<_, i64>(13)? != 0,
            })
        })?
        .collect()
}

/// A key, as its hex or an ID (`#abc...` or not) of one this server has
/// seen (uploading, giving encores, reporting, as an admin).
fn resolve_key(db: &Connection, text: &str) -> Result<String, (u16, String)> {
    let text = text.trim().trim_start_matches('#').to_lowercase();
    if text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(text);
    }
    let keys: Vec<String> = db
        .prepare(
            "SELECT author_key FROM scripts WHERE author_key IS NOT NULL UNION SELECT key FROM encores
             UNION SELECT reporter_key FROM reports UNION SELECT key FROM admins UNION SELECT target FROM bans",
        )
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .map_err(db_error)?;
    keys.into_iter()
        .find(|k| k.len() == 64 && key_id(k) == text)
        .ok_or((404, format!("no key with the ID #{text} has been seen here: give the whole key (hex)")))
}

/// A ban's target: an address, or a key (`resolve_key`).
fn resolve_target(db: &Connection, text: &str) -> Result<String, (u16, String)> {
    match text.trim().parse::<IpAddr>() {
        Ok(ip) => Ok(ip.to_string()),
        Err(_) => resolve_key(db, text),
    }
}

/// Forget addresses older than `KEEP_ADDRESSES` (bans keep theirs).
pub(super) fn forget_old_addresses(state: &State) {
    let before = now() - KEEP_ADDRESSES;
    let db = state.db();
    let result = db.execute_batch(&format!(
        "UPDATE versions SET uploader_ip = NULL WHERE created < {before} AND uploader_ip IS NOT NULL;
         UPDATE encores SET ip = NULL WHERE created < {before} AND ip IS NOT NULL;
         UPDATE reports SET ip = NULL WHERE created < {before} AND ip IS NOT NULL;"
    ));
    if let Err(e) = result {
        println!("couldn't forget old addresses: {e}");
    }
}
