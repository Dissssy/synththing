//! Rotation and recovery (docs/SERVER.md, Authorities).
//!
//! Every server keeps the rotations it's been shown (`apply`): checked
//! against the authorities it trusts, each moves what the old key had
//! (scripts, encores, reports, admin rights, bans) to the new one, and the
//! old key is refused from then on.
//!
//! An authority (`"authority": true`) also issues them: a key-signed
//! rotation (`rotate`), and with mail on, a recovery confirmed by a code
//! sent to the email attached to the identity. Addresses are kept only as
//! a hash (Argon2id, salted with a secret of the server's in
//! `email.pepper`), so a recovery is like a password check: the address
//! typed is hashed to find the identity, and the code goes to it. Adding,
//! changing or taking off an email needs a code sent to the current one,
//! so a stolen key can't take recovery away. Every code's email has a
//! "that wasn't me" link that cancels it: a recovery's pauses recovery for
//! the identity for a day, an email change's pauses changes to its email
//! for a day, and either counts against the address that asked (three in
//! a week ban it for a week).

use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use tiny_http::{Request, Response};

use super::mail::code_mail;
use super::{error, header, json, now, read_body, signer, Reply, State};
use crate::library::rotation::{
    is_email, new_key_message, normalize_email, rotation_message, verify_chain, EmailConfirm, EmailRequest,
    IdentityStatus, RecoverConfirm, RecoverRequest, RotateRequest, Rotation,
};
use crate::library::{base32, hex, key_id, sign_hex, verify_hex, REQUEST_WINDOW_SECONDS};

pub(super) const SCHEMA_V8: &str = "
BEGIN;
CREATE TABLE rotations (
    old TEXT PRIMARY KEY,           -- refused from now on
    new TEXT NOT NULL,
    time INTEGER NOT NULL,
    how TEXT NOT NULL,
    authority TEXT NOT NULL,
    signature TEXT NOT NULL,
    applied INTEGER NOT NULL
);
CREATE TABLE identities (           -- an authority's: keys with an email
    key TEXT PRIMARY KEY,
    email_hash TEXT,
    recovery_paused_until INTEGER,
    changes_paused_until INTEGER    -- email changes, after one was cancelled
);
CREATE UNIQUE INDEX identities_email ON identities(email_hash) WHERE email_hash IS NOT NULL;
CREATE TABLE codes (
    key TEXT NOT NULL,              -- the identity (its current key)
    purpose TEXT NOT NULL,          -- attach, current, recover, delete
    code_hash TEXT NOT NULL,
    email_hash TEXT,                -- attach: the address being attached
    new_key TEXT,                   -- recover: the key recovering to
    cancel_token TEXT,              -- the email's cancel link
    ip TEXT,
    created INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (key, purpose)
);
CREATE TABLE recoveries (key TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE strikes (ip TEXT NOT NULL, created INTEGER NOT NULL);
PRAGMA user_version = 8;
COMMIT;
";

/// How long a code works, and how many tries it gets.
const CODE_SECONDS: i64 = 30 * 60;
const CODE_ATTEMPTS: i64 = 5;
/// Recoveries a day, per identity and per address asking; email changes
/// a day per address.
const RECOVERIES_PER_DAY: usize = 3;
const RECOVERY_ASKS_PER_DAY: usize = 5;
const EMAILS_PER_DAY: usize = 10;
const DAY: i64 = 24 * 60 * 60;

/// Whether `key` was rotated away from: it's refused.
pub(super) fn is_retired(db: &Connection, key: &str) -> bool {
    db.query_row("SELECT 1 FROM rotations WHERE old = ?", [key], |_| Ok(())).optional().ok().flatten().is_some()
}

/// The rotations that followed `key`, oldest first (the records applied
/// here).
pub(super) fn chain_from(db: &Connection, key: &str) -> Vec<Rotation> {
    let mut chain: Vec<Rotation> = Vec::new();
    let mut at = key.to_string();
    while chain.len() < 64 {
        let next: Option<Rotation> = db
            .query_row("SELECT old, new, time, how, authority, signature FROM rotations WHERE old = ?", [&at], |r| {
                Ok(Rotation {
                    old: r.get(0)?,
                    new: r.get(1)?,
                    time: r.get(2)?,
                    how: r.get(3)?,
                    authority: r.get(4)?,
                    signature: r.get(5)?,
                })
            })
            .optional()
            .ok()
            .flatten();
        let Some(record) = next else { break };
        at = record.new.clone();
        chain.push(record);
    }
    chain
}

/// What `key` is now, after any rotations.
pub(super) fn current_key(db: &Connection, key: &str) -> String {
    chain_from(db, key).last().map_or_else(|| key.to_string(), |r| r.new.clone())
}

/// Whether `key` has an email attached here (an authority's own record).
pub(super) fn has_email(db: &Connection, key: &str) -> bool {
    db.query_row("SELECT 1 FROM identities WHERE key = ? AND email_hash IS NOT NULL", [key], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

/// The authorities this server takes rotations from.
fn trusted(state: &State) -> Vec<String> {
    let mut trusted = state.config.authorities.clone();
    if state.config.authority {
        trusted.push(state.key_hex.clone());
    }
    trusted
}

/// Move what `record.old` had to `record.new`, once; false if it was
/// applied before.
pub(super) fn apply_rotation(db: &Connection, record: &Rotation) -> rusqlite::Result<bool> {
    let tx = db.unchecked_transaction()?;
    if tx.query_row("SELECT 1 FROM rotations WHERE old = ?", [&record.old], |_| Ok(())).optional()?.is_some() {
        return Ok(false);
    }
    let (old, new) = (record.old.as_str(), record.new.as_str());
    tx.execute("UPDATE OR IGNORE scripts SET author_key = ?1, author_id = ?2 WHERE author_key = ?3", params![new, key_id(new), old])?;
    tx.execute("UPDATE OR IGNORE encores SET key = ?1 WHERE key = ?2", params![new, old])?;
    tx.execute("DELETE FROM encores WHERE key = ?", [old])?;
    tx.execute("UPDATE scripts SET encores = (SELECT COUNT(*) FROM encores WHERE script_id = scripts.id)", [])?;
    tx.execute("UPDATE reports SET reporter_key = ?1 WHERE reporter_key = ?2", params![new, old])?;
    tx.execute("UPDATE OR IGNORE admins SET key = ?1 WHERE key = ?2", params![new, old])?;
    tx.execute("DELETE FROM admins WHERE key = ?", [old])?;
    // (A ban comes along: a rotation is no way out of one.)
    tx.execute(
        "INSERT OR REPLACE INTO bans (target, until, reason, created, spreads)
         SELECT ?1, until, reason, created, spreads FROM bans WHERE target = ?2 AND (until IS NULL OR until > ?3)",
        params![new, old, now()],
    )?;
    tx.execute("UPDATE OR IGNORE identities SET key = ?1 WHERE key = ?2", params![new, old])?;
    tx.execute("DELETE FROM codes WHERE key = ?", [old])?;
    tx.execute(
        "INSERT INTO rotations (old, new, time, how, authority, signature, applied) VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![old, new, record.time, record.how, record.authority, record.signature, now()],
    )?;
    tx.commit()?;
    Ok(true)
}

/// A record of `old` becoming `new`, signed with this server's key, and
/// applied here.
fn issue(state: &State, db: &Connection, old: &str, new: &str, how: &str) -> Result<Rotation, Reply> {
    let time = now();
    let signature = sign_hex(&state.key, &rotation_message(&state.key_hex, old, new, time, how));
    let record =
        Rotation { old: old.into(), new: new.into(), time, how: how.into(), authority: state.key_hex.clone(), signature };
    apply_rotation(db, &record).map_err(|e| error(500, format!("database: {e}")))?;
    println!("rotated #{} to #{} ({how})", key_id(old), key_id(new));
    Ok(record)
}

fn need_authority(state: &State) -> Result<(), Reply> {
    if state.config.authority { Ok(()) } else { Err(error(404, "this server isn't an identity authority")) }
}

fn need_mail(state: &State) -> Result<(), Reply> {
    need_authority(state)?;
    if state.mailer.on() { Ok(()) } else { Err(error(503, "this server doesn't send email")) }
}

/// A signed request's key and its JSON body.
fn signed_json<T: serde::de::DeserializeOwned>(state: &State, request: &mut Request, path: &str) -> Result<(String, T), Reply> {
    let body = read_body(request, 16 * 1024)?;
    let key = signer(state, request, "POST", path, &body)?.ok_or_else(|| error(401, "that needs your signature"))?;
    let value = serde_json::from_slice(&body).map_err(|e| error(400, format!("not understood: {e}")))?;
    Ok((key, value))
}

fn unsigned_json<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, Reply> {
    let body = read_body(request, 16 * 1024)?;
    serde_json::from_slice(&body).map_err(|e| error(400, format!("not understood: {e}")))
}

/// The new key holds its secret: it signed `new_key_message`, recently.
fn check_new_key(state: &State, old: &str, new: &str, time: i64, signature: &str) -> Result<(), Reply> {
    if (now() - time).abs() > REQUEST_WINDOW_SECONDS {
        return Err(error(401, "the request's time is too far from the server's: is the computer's clock right?"));
    }
    if !verify_hex(new, &new_key_message(&state.key_hex, old, new, time), signature) {
        return Err(error(401, "the new key's signature doesn't match"));
    }
    Ok(())
}

/// A key never seen here: one to rotate to.
fn fresh(db: &Connection, key: &str) -> rusqlite::Result<bool> {
    let seen: Option<i64> = db
        .query_row(
            "SELECT 1 FROM scripts WHERE author_key = ?1 UNION SELECT 1 FROM encores WHERE key = ?1
             UNION SELECT 1 FROM rotations WHERE old = ?1 OR new = ?1 UNION SELECT 1 FROM identities WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(seen.is_none())
}

/// `GET /api/v1/identity/rotations?key=`: the rotations that led to `key`,
/// oldest first (what another server needs to see to move everything the
/// identity's older keys had).
pub(super) fn lineage(state: &State, key: &str) -> Reply {
    let db = state.db();
    let mut chain: Vec<Rotation> = Vec::new();
    let mut at = key.to_string();
    while chain.len() < 64 {
        let found: Option<Rotation> = db
            .query_row("SELECT old, new, time, how, authority, signature FROM rotations WHERE new = ?", [&at], |r| {
                Ok(Rotation {
                    old: r.get(0)?,
                    new: r.get(1)?,
                    time: r.get(2)?,
                    how: r.get(3)?,
                    authority: r.get(4)?,
                    signature: r.get(5)?,
                })
            })
            .optional()
            .ok()
            .flatten();
        let Some(record) = found else { break };
        at = record.old.clone();
        chain.insert(0, record);
    }
    json(200, &chain)
}

/// `POST /api/v1/identity/rotate`: signed by the old key.
pub(super) fn rotate(state: &State, request: &mut Request) -> Reply {
    let result = (|| {
        need_authority(state)?;
        let (old, ask): (String, RotateRequest) = signed_json(state, request, "/api/v1/identity/rotate")?;
        check_new_key(state, &old, &ask.new, ask.time, &ask.new_signature)?;
        let db = state.db();
        if !fresh(&db, &ask.new).map_err(|e| error(500, format!("database: {e}")))? {
            return Err(error(400, "that key has been used already: rotate to a new one"));
        }
        issue(state, &db, &old, &ask.new, "key")
    })();
    match result {
        Ok(record) => json(200, &record),
        Err(reply) => reply,
    }
}

/// `POST /api/v1/identity/apply`: rotations to take (any server).
pub(super) fn apply(state: &State, request: &mut Request, ip: &str) -> Reply {
    let chain: Vec<Rotation> = match unsigned_json(request) {
        Ok(chain) => chain,
        Err(reply) => return reply,
    };
    if chain.len() > 64 {
        return error(400, "that's a lot of rotations");
    }
    if let Err(reply) = super::moderation::limit(state, "apply", ip, 60, Duration::from_secs(3600), "requests") {
        return reply;
    }
    if let Err(problem) = verify_chain(&chain, &trusted(state)) {
        return error(400, problem);
    }
    let db = state.db();
    let mut applied = 0;
    for record in &chain {
        match apply_rotation(&db, record) {
            Ok(true) => {
                applied += 1;
                println!("applied the rotation of #{} to #{}", key_id(&record.old), key_id(&record.new));
            }
            Ok(false) => {}
            Err(e) => return error(500, format!("database: {e}")),
        }
    }
    json(200, &serde_json::json!({ "applied": applied }))
}

/// The hash an address is kept as.
pub(super) fn email_hash(state: &State, address: &str) -> String {
    let mut out = [0u8; 32];
    let params = argon2::Params::new(19 * 1024, 2, 1, Some(32)).expect("valid Argon2 parameters");
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let _ = argon.hash_password_into(normalize_email(address).as_bytes(), &state.pepper, &mut out);
    hex(&out)
}

fn code_hash(code: &str) -> String {
    let normal: String = code.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_uppercase();
    hex(&Sha256::digest(normal.as_bytes()))
}

/// A code: eight letters and digits, `ABCD-EFGH`.
pub(super) fn new_code() -> Result<String, Reply> {
    let mut bytes = [0u8; 5];
    getrandom::fill(&mut bytes).map_err(|_| error(500, "couldn't make a code"))?;
    let code = base32(&bytes).to_uppercase();
    Ok(format!("{}-{}", &code[..4], &code[4..]))
}

/// Store a code for `key` and `purpose` (replacing one sent before).
#[allow(clippy::too_many_arguments)]
pub(super) fn store_code(
    db: &Connection,
    key: &str,
    purpose: &str,
    code: &str,
    email_hash: Option<&str>,
    new_key: Option<&str>,
    cancel_token: Option<&str>,
    ip: &str,
) -> Result<(), Reply> {
    db.execute(
        "INSERT OR REPLACE INTO codes (key, purpose, code_hash, email_hash, new_key, cancel_token, ip, created, attempts)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)",
        params![key, purpose, code_hash(code), email_hash, new_key, cancel_token, ip, now()],
    )
    .map(|_| ())
    .map_err(|e| error(500, format!("database: {e}")))
}

/// Check `code` against `key`'s code for `purpose`: the row (email hash,
/// new key), or why not. A wrong code uses up a try.
pub(super) fn check_code(db: &Connection, key: &str, purpose: &str, code: &str) -> Result<(Option<String>, Option<String>), Reply> {
    // (Its hash, email hash, new key, when, tries.)
    type Row = (String, Option<String>, Option<String>, i64, i64);
    let row: Option<Row> = db
        .query_row(
            "SELECT code_hash, email_hash, new_key, created, attempts FROM codes WHERE key = ? AND purpose = ?",
            params![key, purpose],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|e| error(500, format!("database: {e}")))?;
    let Some((hash, email, new_key, created, attempts)) = row else {
        return Err(error(400, "there's no code waiting: ask for a new one"));
    };
    if now() - created > CODE_SECONDS || attempts >= CODE_ATTEMPTS {
        let _ = db.execute("DELETE FROM codes WHERE key = ? AND purpose = ?", params![key, purpose]);
        return Err(error(400, "that code has expired: ask for a new one"));
    }
    if hash != code_hash(code) {
        let _ = db.execute("UPDATE codes SET attempts = attempts + 1 WHERE key = ? AND purpose = ?", params![key, purpose]);
        return Err(error(400, "that code isn't right"));
    }
    Ok((email, new_key))
}

/// `POST /api/v1/identity/status`: signed.
pub(super) fn status(state: &State, request: &mut Request) -> Reply {
    let result = (|| {
        need_authority(state)?;
        let (key, _): (String, serde_json::Value) = signed_json(state, request, "/api/v1/identity/status")?;
        Ok(IdentityStatus { email: has_email(&state.db(), &key), mail: state.mailer.on() })
    })();
    match result {
        Ok(status) => json(200, &status),
        Err(reply) => reply,
    }
}

/// A token for a code's "that wasn't me" link, and the link (none without
/// a `public_url`).
pub(super) fn cancel_link(state: &State) -> Result<(String, Option<String>), Reply> {
    let mut token = [0u8; 16];
    getrandom::fill(&mut token).map_err(|_| error(500, "couldn't make a link"))?;
    let token = hex(&token);
    let base = state.config.public_url.trim_end_matches('/');
    let link = (!base.is_empty()).then(|| format!("{base}/api/v1/identity/cancel?token={token}"));
    Ok((token, link))
}

/// A strike against `ip` (a cancelled code): three in a week ban it for a
/// week.
fn strike(db: &Connection, ip: &str) {
    let _ = db.execute("INSERT INTO strikes (ip, created) VALUES (?, ?)", params![ip, now()]);
    let strikes: i64 = db
        .query_row("SELECT COUNT(*) FROM strikes WHERE ip = ? AND created > ?", params![ip, now() - 7 * DAY], |r| r.get(0))
        .unwrap_or(0);
    if strikes >= 3 {
        let _ = db.execute(
            "INSERT OR REPLACE INTO bans (target, until, reason, created) VALUES (?, ?, ?, ?)",
            params![ip, now() + 7 * DAY, "codes cancelled by the people they were sent to", now()],
        );
        println!("banned {ip} for a week (cancelled codes)");
    }
}

/// `POST /api/v1/identity/email`: signed; send the codes.
pub(super) fn email(state: &State, request: &mut Request, ip: &str) -> Reply {
    let result = (|| {
        need_mail(state)?;
        let (key, ask): (String, EmailRequest) = signed_json(state, request, "/api/v1/identity/email")?;
        super::moderation::limit(state, "email", ip, EMAILS_PER_DAY, Duration::from_secs(DAY as u64), "email requests")?;
        let db = state.db();
        let (attached, paused): (Option<String>, Option<i64>) = db
            .query_row("SELECT email_hash, changes_paused_until FROM identities WHERE key = ?", [&key], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(|e| error(500, format!("database: {e}")))?
            .unwrap_or_default();
        if paused.is_some_and(|until| until > now()) {
            return Err(error(
                429,
                "changes to your identity's email are paused for a day (one was cancelled from its email)",
            ));
        }
        let mut sent = Vec::new();
        let mut mails = Vec::new();
        if let Some(attached) = &attached {
            let Some(current) = ask.current.as_deref().filter(|c| email_hash(state, c) == *attached) else {
                return Err(error(403, "that isn't the address attached to your identity"));
            };
            let code = new_code()?;
            let (token, link) = cancel_link(state)?;
            store_code(&db, &key, "current", &code, None, None, Some(&token), ip)?;
            let what = if ask.address.is_some() { "to change your identity's email" } else { "to take the email off your identity" };
            let cancel = link.as_deref().map(|l| (l, "which also pauses changes to your identity's email for a day"));
            mails.push(code_mail(current.trim(), &state.config.name, &code, what, cancel));
            sent.push("current");
        } else {
            let _ = db.execute("DELETE FROM codes WHERE key = ? AND purpose = 'current'", [&key]);
        }
        match &ask.address {
            Some(address) => {
                if !is_email(address) {
                    return Err(error(400, "that isn't an email address"));
                }
                let hash = email_hash(state, address);
                let taken: Option<String> = db
                    .query_row("SELECT key FROM identities WHERE email_hash = ?", [&hash], |r| r.get(0))
                    .optional()
                    .map_err(|e| error(500, format!("database: {e}")))?;
                if taken.is_some_and(|other| other != key) {
                    return Err(error(409, "that address is attached to another identity"));
                }
                let code = new_code()?;
                let (token, link) = cancel_link(state)?;
                store_code(&db, &key, "attach", &code, Some(&hash), None, Some(&token), ip)?;
                let cancel = link
                    .as_deref()
                    .map(|l| (l, "and this address won't be attached (whoever asked can't try again for a day)"));
                mails.push(code_mail(address.trim(), &state.config.name, &code, "to attach this address to your identity", cancel));
                sent.push("new");
            }
            None if attached.is_none() => return Err(error(400, "there's no email attached to take off")),
            None => {
                let _ = db.execute("DELETE FROM codes WHERE key = ? AND purpose = 'attach'", [&key]);
            }
        }
        drop(db);
        for mail in mails {
            state.mailer.send(mail).map_err(|e| error(502, e))?;
        }
        Ok(sent)
    })();
    match result {
        Ok(sent) => json(200, &serde_json::json!({ "sent": sent })),
        Err(reply) => reply,
    }
}

/// `POST /api/v1/identity/email/confirm`: signed; the codes, and done.
pub(super) fn email_confirm(state: &State, request: &mut Request) -> Reply {
    let result = (|| {
        need_mail(state)?;
        let (key, confirm): (String, EmailConfirm) = signed_json(state, request, "/api/v1/identity/email/confirm")?;
        let db = state.db();
        let attached = has_email(&db, &key);
        if attached {
            check_code(&db, &key, "current", confirm.current_code.as_deref().unwrap_or_default())?;
        }
        let waiting = db
            .query_row("SELECT 1 FROM codes WHERE key = ? AND purpose = 'attach'", [&key], |_| Ok(()))
            .optional()
            .map_err(|e| error(500, format!("database: {e}")))?
            .is_some();
        let hash = if waiting {
            check_code(&db, &key, "attach", confirm.code.as_deref().unwrap_or_default())?.0
        } else if attached {
            None
        } else {
            return Err(error(400, "there's no code waiting: ask for a new one"));
        };
        db.execute(
            "INSERT INTO identities (key, email_hash) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET email_hash = ?2",
            params![key, hash],
        )
        .map_err(|e| error(409, format!("couldn't attach it: {e}")))?;
        let _ = db.execute("DELETE FROM codes WHERE key = ? AND purpose IN ('attach', 'current')", [&key]);
        println!("#{}: email {}", key_id(&key), if hash.is_some() { "attached" } else { "taken off" });
        Ok(hash.is_some())
    })();
    match result {
        Ok(email) => json(200, &IdentityStatus { email, mail: true }),
        Err(reply) => reply,
    }
}

/// `POST /api/v1/identity/recover`: unsigned. The answer's the same
/// whether the address is attached to anything or not.
pub(super) fn recover(state: &State, request: &mut Request, ip: &str) -> Reply {
    let result = (|| {
        need_mail(state)?;
        let ask: RecoverRequest = unsigned_json(request)?;
        check_new_key(state, "", &ask.new, ask.time, &ask.new_signature)?;
        if !is_email(&ask.address) {
            return Err(error(400, "that isn't an email address"));
        }
        super::moderation::refuse_banned(state, None, ip)?;
        super::moderation::limit(state, "recover", ip, RECOVERY_ASKS_PER_DAY, Duration::from_secs(DAY as u64), "recoveries")?;
        let db = state.db();
        if !fresh(&db, &ask.new).map_err(|e| error(500, format!("database: {e}")))? {
            return Err(error(400, "that key has been used already: recover to a new one"));
        }
        let hash = email_hash(state, &ask.address);
        let found: Option<(String, Option<i64>)> = db
            .query_row("SELECT key, recovery_paused_until FROM identities WHERE email_hash = ?", [&hash], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(|e| error(500, format!("database: {e}")))?;
        let Some((key, paused)) = found else { return Ok(()) };
        // Paused (a recovery was cancelled), or asked for too often: no
        // email, and the same answer as ever (saying so would tell anyone
        // the address is attached to an identity).
        let recent: i64 = db
            .query_row("SELECT COUNT(*) FROM recoveries WHERE key = ? AND created > ?", params![key, now() - DAY], |r| r.get(0))
            .map_err(|e| error(500, format!("database: {e}")))?;
        if paused.is_some_and(|until| until > now()) || recent >= RECOVERIES_PER_DAY as i64 {
            println!("recovery of #{} not sent (paused, or too many today)", key_id(&key));
            return Ok(());
        }
        let code = new_code()?;
        let (token, link) = cancel_link(state)?;
        store_code(&db, &key, "recover", &code, None, Some(&ask.new), Some(&token), ip)?;
        let _ = db.execute("INSERT INTO recoveries (key, created) VALUES (?, ?)", params![key, now()]);
        drop(db);
        let cancel = link.as_deref().map(|l| (l, "which also pauses recovery of your identity for a day"));
        let mail = code_mail(ask.address.trim(), &state.config.name, &code, "to recover your identity (with a new key)", cancel);
        state.mailer.send(mail).map_err(|e| error(502, e))?;
        println!("recovery of #{} asked for from {ip}", key_id(&key));
        Ok(())
    })();
    match result {
        Ok(()) => json(200, &serde_json::json!({ "sent": "if that address is attached to an identity, a code is on its way" })),
        Err(reply) => reply,
    }
}

/// `POST /api/v1/identity/recover/confirm`: unsigned; the code, and the
/// rotation to the new key.
pub(super) fn recover_confirm(state: &State, request: &mut Request) -> Reply {
    let result = (|| {
        need_mail(state)?;
        let confirm: RecoverConfirm = unsigned_json(request)?;
        let db = state.db();
        let key: Option<String> = db
            .query_row("SELECT key FROM codes WHERE purpose = 'recover' AND new_key = ?", [&confirm.new], |r| r.get(0))
            .optional()
            .map_err(|e| error(500, format!("database: {e}")))?;
        let Some(key) = key else { return Err(error(400, "there's no recovery waiting for that key")) };
        check_code(&db, &key, "recover", &confirm.code)?;
        issue(state, &db, &key, &confirm.new, "email")
    })();
    match result {
        Ok(record) => json(200, &record),
        Err(reply) => reply,
    }
}

fn page(status: u16, text: &str) -> Reply {
    let html = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>synththing</title>\
         <body style=\"font-family:-apple-system,Segoe UI,Helvetica,Arial,sans-serif;max-width:480px;margin:48px auto;\
         padding:0 16px;color:#1d1d24\"><p style=\"color:#6b5bd6;font-weight:600\">synththing</p><p>{text}</p></body>"
    );
    Response::from_data(html.into_bytes())
        .with_status_code(status)
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
}

/// `GET /api/v1/identity/cancel?token=`: a code email's "that wasn't me"
/// link.
pub(super) fn cancel(state: &State, token: &str) -> Reply {
    let db = state.db();
    let found: Option<(String, String, Option<String>)> = db
        .query_row("SELECT key, purpose, ip FROM codes WHERE cancel_token = ?", [token], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .ok()
        .flatten();
    let Some((key, purpose, ip)) = found.filter(|_| !token.is_empty()) else {
        return page(404, "That link has been used already, or the code expired: nothing more to do.");
    };
    if let Some(ip) = &ip {
        strike(&db, ip);
    }
    if purpose == "recover" {
        let _ = db.execute("DELETE FROM codes WHERE key = ? AND purpose = 'recover'", [&key]);
        let _ = db.execute("UPDATE identities SET recovery_paused_until = ? WHERE key = ?", params![now() + DAY, key]);
        println!("recovery of #{} cancelled", key_id(&key));
        return page(
            200,
            "Cancelled: that code doesn't work any more, and nobody can ask to recover your identity for the next \
             day. Your key and your email are as they were.",
        );
    }
    let _ = db.execute("DELETE FROM codes WHERE key = ? AND purpose IN ('attach', 'current', 'delete')", [&key]);
    let _ = db.execute(
        "INSERT INTO identities (key, changes_paused_until) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET changes_paused_until = ?2",
        params![key, now() + DAY],
    );
    println!("{purpose} for #{} cancelled", key_id(&key));
    page(
        200,
        match purpose.as_str() {
            "attach" => "Cancelled: your address won't be attached, and that identity can't try again for the next day.",
            "delete" => {
                "Cancelled: nothing of your identity's is deleted, and nobody can ask to delete it for the next day."
            }
            _ => "Cancelled: your identity's email stays as it is, and it can't be changed or taken off for the next day.",
        },
    )
}
