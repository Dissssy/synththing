//! Songs (docs/SERVER.md): MIDI files, kept beside scripts in the same
//! table (`kind = 'song'`), so encores, reports, moderation, notices,
//! notifications and deletion cover them as they are; their own details
//! (composer, the rights declared, the file's facts) in `songs`, the file
//! itself in its version's `data`. Only on a server that takes them
//! (`"songs": true`). Listings are scripts unless songs are asked for, and
//! the script source endpoint refuses songs, so an older app never tries
//! to install one as a script.

use rusqlite::{params, Connection, OptionalExtension};
use tiny_http::{Request, Response};

use super::{count_upload, error, header, json, now, read_body_or, signed_receipt, signer, upload_allowance, Reply, State};
use crate::library::fingerprint::{Fingerprint, COPY};
use crate::library::songs::{self, SongInfo, SongUpload};
use crate::library::{
    key_id, sha256_hex, sign_hex, source_message, SHA256_HEADER, SIGNATURE_HEADER, VERSION_HEADER,
};

pub(super) const SCHEMA_V12: &str = "
BEGIN;
ALTER TABLE scripts ADD COLUMN kind TEXT NOT NULL DEFAULT 'script';
CREATE INDEX scripts_kind ON scripts(kind);
ALTER TABLE versions ADD COLUMN data BLOB;
CREATE TABLE songs (
    id TEXT PRIMARY KEY REFERENCES scripts(id) ON DELETE CASCADE,
    composer TEXT NOT NULL,
    arranger TEXT NOT NULL,
    from_title TEXT NOT NULL,
    copyright TEXT,
    rights TEXT NOT NULL,
    license TEXT NOT NULL,
    made_for TEXT,
    length REAL NOT NULL,
    notes INTEGER NOT NULL,
    tracks INTEGER NOT NULL,
    channels INTEGER NOT NULL,
    preview_start REAL NOT NULL
);
PRAGMA user_version = 12;
COMMIT;
";

/// The oldest app that knows songs.
pub(super) const SONGS_SINCE: &str = "0.4.8";

/// A song's details, if `id` is one.
pub(super) fn info(db: &Connection, id: &str) -> Option<SongInfo> {
    db.query_row(
        "SELECT composer, arranger, from_title, copyright, rights, license, made_for, length, notes, tracks, channels,
         preview_start FROM songs WHERE id = ?",
        [id],
        |r| {
            Ok(SongInfo {
                composer: r.get(0)?,
                arranger: r.get(1)?,
                from_title: r.get(2)?,
                copyright: r.get(3)?,
                rights: r.get(4)?,
                license: r.get(5)?,
                made_for: r.get(6)?,
                length: r.get(7)?,
                notes: r.get::<_, i64>(8)?.max(0) as u64,
                tracks: r.get::<_, i64>(9)?.clamp(0, u16::MAX as i64) as u16,
                channels: r.get::<_, i64>(10)?.clamp(0, u16::MAX as i64) as u16,
                preview_start: r.get(11)?,
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

/// Why a song's notes can't be taken: someone else's song (nearly), or one
/// taken down for good.
fn refuse_copy(db: &Connection, fingerprint: &Fingerprint, notes: u64, uploader: Option<&str>, same: Option<&str>) -> rusqlite::Result<Option<String>> {
    // (Too short to tell apart.)
    if notes < 40 {
        return Ok(None);
    }
    if super::copyright::blocked(db, fingerprint)? {
        return Ok(Some("it's the same as a song taken down after a copyright notice".into()));
    }
    let rows: Vec<(String, String, String, String, Option<String>)> = db
        .prepare(
            "SELECT v.fingerprint, s.id, s.name, s.author_name, s.author_key FROM versions v JOIN scripts s ON s.id = v.script_id
             WHERE s.kind = 'song' AND v.fingerprint IS NOT NULL",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (stored, id, name, by, author) in rows {
        let own = uploader.is_some() && author.as_deref() == uploader;
        if own || same == Some(id.as_str()) {
            continue;
        }
        if Fingerprint::from_hex(&stored).is_some_and(|f| f.similarity(fingerprint) >= COPY) {
            return Ok(Some(format!("it's the same song as \"{name}\" by {by}")));
        }
    }
    Ok(None)
}

/// `POST /api/v1/songs`: a song, signed or (on an open server) not.
pub(super) fn upload(state: &State, request: &mut Request, ip: &str) -> Reply {
    let result = (|| {
        if !state.config.songs {
            return Err(error(403, "this server doesn't take songs"));
        }
        let too_big = format!("too big: songs can be up to {} MB", songs::MAX_SONG_BYTES / 1024 / 1024);
        let body = read_body_or(request, songs::MAX_SONG_BYTES * 4 / 3 + 64 * 1024, &too_big)?;
        let author = signer(state, request, "POST", "/api/v1/songs", &body)?;
        if author.is_none() && state.config.mode != "open" {
            return Err(error(403, "this server only takes signed uploads"));
        }
        super::moderation::refuse_banned(state, author.as_deref(), ip)?;
        let mut upload: SongUpload = serde_json::from_slice(&body).map_err(|e| error(400, format!("not a song: {e}")))?;
        upload.name = upload.name.trim().to_string();
        upload.author_name = upload.author_name.trim().to_string();
        upload.arranger = upload.arranger.trim().to_string();
        if upload.arranger.is_empty() {
            upload.arranger = upload.author_name.clone();
        }
        upload.tags = crate::library::parse_tags(&upload.tags.join(","));
        songs::check_upload(&upload).map_err(|e| error(400, e))?;
        let bytes = songs::decode(&upload.data).map_err(|e| error(400, e))?;
        let facts = songs::analyze(&bytes).map_err(|e| error(400, e))?;
        let slug = match (&author, upload.slug.take()) {
            (Some(_), Some(slug)) if crate::library::is_slug(&slug) => Some(slug),
            (Some(_), _) => return Err(error(400, "a signed upload needs a slug")),
            (None, _) => None,
        };
        let sha256 = sha256_hex(&bytes);
        let mut db = state.db();
        if let Some(script) = &upload.made_for {
            let exists: Option<i64> = db
                .query_row("SELECT 1 FROM scripts WHERE id = ? AND kind = 'script' AND hidden = 0", [script], |r| r.get(0))
                .optional()
                .map_err(|e| error(500, format!("database: {e}")))?;
            if exists.is_none() {
                return Err(error(400, "the script it's made for isn't here"));
            }
        }
        let existing: Option<(String, u32, String)> = match (&author, &slug) {
            (Some(key), Some(slug)) => db
                .query_row("SELECT id, latest, kind FROM scripts WHERE author_key = ? AND slug = ?", params![key, slug], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()
                .map_err(|e| error(500, format!("database: {e}")))?,
            _ => None,
        };
        if existing.as_ref().is_some_and(|(_, _, kind)| kind != "song") {
            return Err(error(409, "you have a script with that slug: pick another"));
        }
        if let Some((id, latest, _)) = &existing {
            let same: Option<String> = db
                .query_row("SELECT sha256 FROM versions WHERE script_id = ? AND version = ?", params![id, latest], |r| r.get(0))
                .optional()
                .map_err(|e| error(500, format!("database: {e}")))?;
            if same.as_deref() == Some(sha256.as_str()) {
                return Ok((201, signed_receipt(state, id.clone(), *latest, sha256)));
            }
        }
        let same = existing.as_ref().map(|(id, ..)| id.as_str());
        if let Some(why) = refuse_copy(&db, &facts.fingerprint, facts.notes, author.as_deref(), same)
            .map_err(|e| error(500, format!("database: {e}")))?
        {
            return Err(error(400, why));
        }
        let limited = upload_allowance(state, &db, author.as_deref(), ip)?;
        let id = match &existing {
            Some((id, ..)) => id.clone(),
            None => loop {
                let mut bytes = [0u8; 5];
                getrandom::fill(&mut bytes).map_err(|_| error(500, "couldn't make an ID"))?;
                let id = crate::library::base32(&bytes);
                let taken: Option<i64> = db
                    .query_row("SELECT 1 FROM scripts WHERE id = ?", [&id], |r| r.get(0))
                    .optional()
                    .map_err(|e| error(500, format!("database: {e}")))?;
                if taken.is_none() {
                    break id;
                }
            },
        };
        let version = existing.as_ref().map_or(1, |(_, latest, _)| latest + 1);
        let author_id = author.as_deref().map(key_id);
        let created = now();
        let stored = db.transaction().and_then(|tx| {
            if existing.is_some() {
                tx.execute(
                    "UPDATE scripts SET name = ?, description = ?, category = ?, tags = ?, author_name = ?, latest = ?, updated = ?
                     WHERE id = ?",
                    params![upload.name, upload.description, upload.from, upload.tags.join(","), upload.author_name, version, created, id],
                )?;
            } else {
                tx.execute(
                    "INSERT INTO scripts (id, name, description, category, tags, author_name, author_key, author_id, slug,
                     latest, created, updated, kind) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 'song')",
                    params![
                        id,
                        upload.name,
                        upload.description,
                        upload.from,
                        upload.tags.join(","),
                        upload.author_name,
                        author,
                        author_id,
                        slug,
                        created,
                        created
                    ],
                )?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO songs (id, composer, arranger, from_title, copyright, rights, license, made_for, length,
                 notes, tracks, channels, preview_start) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    id,
                    upload.composer.trim(),
                    upload.arranger,
                    upload.from_title.trim(),
                    facts.copyright,
                    upload.rights,
                    upload.license.trim(),
                    upload.made_for,
                    facts.length,
                    facts.notes as i64,
                    facts.tracks,
                    facts.channels,
                    facts.preview_start
                ],
            )?;
            tx.execute(
                "INSERT INTO versions (script_id, version, sha256, source, app_version, uploader_ip, created, min_app_version,
                 fingerprint, data) VALUES (?, ?, ?, '', ?, ?, ?, ?, ?, ?)",
                params![id, version, sha256, upload.app_version, ip, created, SONGS_SINCE, facts.fingerprint.to_hex(), bytes],
            )?;
            tx.commit()
        });
        stored.map_err(|e| error(500, format!("database: {e}")))?;
        drop(db);
        if limited {
            count_upload(state, ip);
        }
        state.queue_preview(&id, version);
        let who = author_id.map(|id| format!(" (#{id})")).unwrap_or_default();
        println!("uploaded song {id} v{version} \"{}\" by {}{who} from {ip}", upload.name, upload.author_name);
        Ok((201, signed_receipt(state, id, version, sha256)))
    })();
    match result {
        Ok((status, receipt)) => json(status, &receipt),
        Err(reply) => reply,
    }
}

/// `GET /api/v1/songs/{id}/file?version=`: the MIDI file, signed by the
/// server like a script's source.
pub(super) fn file(state: &State, id: &str, version: Option<u32>) -> Reply {
    let db = state.db();
    let found: Option<(u32, String, Vec<u8>)> = db
        .query_row(
            "SELECT v.version, v.sha256, v.data FROM versions v JOIN scripts s ON s.id = v.script_id
             WHERE s.id = ?1 AND s.kind = 'song' AND s.hidden = 0 AND v.version = COALESCE(?2, s.latest)",
            params![id, version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((version, sha256, bytes)) = found else { return error(404, "no such song") };
    super::notifications::downloaded(state, &db, id);
    let signature = sign_hex(&state.key, &source_message(&state.key_hex, id, version, &sha256));
    Response::from_data(bytes)
        .with_header(header("Content-Type", "audio/midi"))
        .with_header(header(VERSION_HEADER, &version.to_string()))
        .with_header(header(SHA256_HEADER, &sha256))
        .with_header(header(SIGNATURE_HEADER, &signature))
}
