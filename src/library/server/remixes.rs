//! Remixes and copy detection (docs/SERVER.md).
//!
//! An upload can say what it's a remix of (`RemixOf`, from its installed
//! copy's sidecar): a script here, found by its ID or by its author and
//! slug (a bundled script's copy knows only those), and the version by the
//! source's SHA-256; or one on another server, recorded as given. The
//! original then lists it among its remixes, and it links back; if the
//! original goes, the remix says it was remixed from a deleted script.
//!
//! Each version's `Fingerprint` is kept. An upload that's a near-copy of
//! someone else's script here (or of a bundled visualizer or game) is
//! refused, unless it's a remix of it, or of something it was remixed from;
//! a remix with no changes at all is refused too. An author's own scripts
//! are never copies of each other.

use std::collections::HashSet;
use std::sync::OnceLock;

use rusqlite::{params, Connection, OptionalExtension};

use crate::library::fingerprint::{tokens, Fingerprint, COPY};
use crate::library::{RemixLink, RemixOf, ScriptSummary, OFFICIAL_PUBLISHER};

pub(super) const SCHEMA_V7: &str = "
BEGIN;
ALTER TABLE versions ADD COLUMN fingerprint TEXT;
ALTER TABLE scripts ADD COLUMN remix_server TEXT;   -- '' for a script here
ALTER TABLE scripts ADD COLUMN remix_id TEXT;
ALTER TABLE scripts ADD COLUMN remix_version INTEGER;
ALTER TABLE scripts ADD COLUMN remix_name TEXT;
ALTER TABLE scripts ADD COLUMN remix_author_id TEXT;
ALTER TABLE scripts ADD COLUMN remix_slug TEXT;
CREATE INDEX scripts_remix ON scripts(remix_id);
PRAGMA user_version = 7;
COMMIT;
";

/// Fingerprint the versions stored before there were fingerprints.
pub(super) fn fingerprint_stored(db: &Connection) -> rusqlite::Result<()> {
    let stored: Vec<(String, u32, String)> = db
        .prepare("SELECT script_id, version, source FROM versions WHERE fingerprint IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, version, source) in stored {
        db.execute(
            "UPDATE versions SET fingerprint = ? WHERE script_id = ? AND version = ?",
            params![Fingerprint::of(&source).to_hex(), id, version],
        )?;
    }
    Ok(())
}

/// What an upload is a remix of, as stored.
pub(super) struct Remix {
    pub server: String,
    pub id: String,
    pub version: Option<u32>,
    pub name: String,
    pub author_id: Option<String>,
    pub slug: Option<String>,
}

/// Where `remix` points, here or elsewhere. A script here that can't be
/// found (deleted) is still recorded, as it was named.
pub(super) fn resolve(db: &Connection, remix: &RemixOf) -> rusqlite::Result<Remix> {
    let as_given = |server: &str| Remix {
        server: server.to_string(),
        id: remix.id.clone(),
        version: None,
        name: remix.name.clone(),
        author_id: remix.author_id.clone(),
        slug: remix.slug.clone(),
    };
    if !remix.server.is_empty() {
        return Ok(as_given(&remix.server));
    }
    type Found = (String, String, Option<String>, Option<String>);
    let columns = "SELECT id, name, author_id, slug FROM scripts";
    let row = |r: &rusqlite::Row| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?));
    let found: Option<Found> = if remix.id.is_empty() {
        match (&remix.author_id, &remix.slug) {
            (Some(author), Some(slug)) => {
                db.query_row(&format!("{columns} WHERE author_id = ? AND slug = ?"), params![author, slug], row).optional()?
            }
            _ => None,
        }
    } else {
        db.query_row(&format!("{columns} WHERE id = ?"), [&remix.id], row).optional()?
    };
    let Some((id, name, author_id, slug)) = found else {
        return Ok(as_given(""));
    };
    let version = db
        .query_row("SELECT version FROM versions WHERE script_id = ? AND sha256 = ?", params![id, remix.sha256], |r| r.get(0))
        .optional()?;
    Ok(Remix { server: String::new(), id, version, name, author_id, slug })
}

/// A script here and what it was remixed from, back up the chain (ending
/// at one that isn't a remix, or one on another server).
fn chain(db: &Connection, first: &str) -> rusqlite::Result<Vec<String>> {
    let mut ids = vec![first.to_string()];
    while ids.len() < 32 {
        let up: Option<Option<String>> = db
            .query_row(
                "SELECT remix_id FROM scripts WHERE id = ? AND (remix_server IS NULL OR remix_server = '')",
                [ids.last().map(String::as_str).unwrap_or_default()],
                |r| r.get(0),
            )
            .optional()?;
        match up.flatten() {
            Some(id) if !id.is_empty() && !ids.contains(&id) => ids.push(id),
            _ => break,
        }
    }
    Ok(ids)
}

/// Whether script `id` was remixed (at some remove) from one of `key`'s.
fn derived_from(db: &Connection, id: &str, key: Option<&str>) -> rusqlite::Result<bool> {
    let Some(key) = key else { return Ok(false) };
    for up in chain(db, id)?.into_iter().skip(1) {
        let author: Option<Option<String>> =
            db.query_row("SELECT author_key FROM scripts WHERE id = ?", [&up], |r| r.get(0)).optional()?;
        if author.flatten().as_deref() == Some(key) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The bundled visualizers and games: (slug, name, fingerprint).
fn bundled() -> &'static [(String, String, Fingerprint)] {
    static BUNDLED: OnceLock<Vec<(String, String, Fingerprint)>> = OnceLock::new();
    BUNDLED.get_or_init(|| {
        crate::lua_visualizer::BUNDLED_SCRIPTS
            .iter()
            .filter(|(category, ..)| matches!(*category, "visualizers" | "games"))
            .map(|&(_, file, source)| {
                let stem = file.trim_end_matches(".lua");
                (crate::library::slugify(stem), crate::lua_visualizer::display_name(std::path::Path::new(file)), Fingerprint::of(source))
            })
            .collect()
    })
}

/// Scripts shorter than this (in tokens) aren't judged copies: there's
/// too little to tell one `function render() ... end` from another.
const MIN_TOKENS: usize = 40;

/// Why `source` (its `fingerprint`), uploaded by `uploader` (as script
/// `same` if it's a new version), can't be taken, if it can't: a copy of
/// someone else's script that isn't credited as a remix, or a remix with
/// no changes.
pub(super) fn refuse_copy(
    db: &Connection,
    source: &str,
    fingerprint: &Fingerprint,
    uploader: Option<&str>,
    same: Option<&str>,
    remix: Option<(&RemixOf, &Remix)>,
) -> rusqlite::Result<Option<String>> {
    // What it may resemble: what it's a remix of, and that one's own
    // originals (and the bundled scripts the official ones among them are).
    let mut credited: HashSet<String> = HashSet::new();
    let mut credited_bundled: HashSet<String> = HashSet::new();
    // (An official script, here or on the official server: the bundled one.)
    if let Some((asked, _)) = remix
        && asked.author_id.as_deref() == Some(crate::library::official_id().as_str())
        && let Some(slug) = &asked.slug
    {
        credited_bundled.insert(slug.clone());
    }
    let remix = remix.map(|(_, resolved)| resolved);
    if let Some(remix) = remix.filter(|r| r.server.is_empty() && !r.id.is_empty()) {
        for id in chain(db, &remix.id)? {
            let official: Option<(Option<String>, Option<String>)> = db
                .query_row("SELECT author_key, slug FROM scripts WHERE id = ?", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            if let Some((Some(key), Some(slug))) = official
                && key == OFFICIAL_PUBLISHER
            {
                credited_bundled.insert(slug);
            }
            credited.insert(id);
        }
        // The very same code as the version remixed (or any of the
        // original's), comments and spacing aside: nothing's been changed.
        let originals: Vec<String> = db
            .prepare("SELECT source FROM versions WHERE script_id = ?")?
            .query_map([&remix.id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let code = tokens(source);
        if originals.iter().any(|original| tokens(original) == code) {
            return Ok(Some(format!(
                "it's the same as \"{}\", the script it's a remix of: change something first",
                remix.name
            )));
        }
    }

    if tokens(source).len() < MIN_TOKENS {
        return Ok(None);
    }
    let mut closest: Option<(f64, String)> = None;
    let mut consider = |similarity: f64, what: String| {
        if similarity >= COPY && closest.as_ref().is_none_or(|(best, _)| similarity > *best) {
            closest = Some((similarity, what));
        }
    };
    let mut statement = db.prepare(
        "SELECT v.fingerprint, s.id, s.name, s.author_name, s.author_key FROM versions v JOIN scripts s ON s.id = v.script_id
         WHERE v.fingerprint IS NOT NULL",
    )?;
    let rows = statement.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?))
    })?;
    let mut matches: Vec<(f64, String, String)> = Vec::new();
    for row in rows {
        let (stored, id, name, author_name, author_key) = row?;
        let own = uploader.is_some() && author_key.as_deref() == uploader;
        if own || same == Some(id.as_str()) || credited.contains(&id) {
            continue;
        }
        if let Some(stored) = Fingerprint::from_hex(&stored) {
            let similarity = stored.similarity(fingerprint);
            if similarity >= COPY {
                matches.push((similarity, id, format!("\"{name}\" by {author_name}")));
            }
        }
    }
    drop(statement);
    // (Someone's remix of the uploader's own script resembles their own
    // work: not a copy of it.)
    for (similarity, id, what) in matches {
        if !derived_from(db, &id, uploader)? {
            consider(similarity, what);
        }
    }
    if uploader != Some(OFFICIAL_PUBLISHER) {
        for (slug, name, bundled) in bundled() {
            if !credited_bundled.contains(slug) {
                consider(bundled.similarity(fingerprint), format!("\"{name}\", which comes with synththing"));
            }
        }
    }
    Ok(closest.map(|(similarity, what)| {
        format!(
            "it's {}% the same as {what}: if it's built on that, publish it as a remix (from your installed copy: \
             Publish remix... by the script picker), so its author is credited",
            (similarity * 100.0).round()
        )
    }))
}

/// A script's remix columns: server, original's ID, version, name, author
/// ID, slug.
type StoredRemix = (Option<String>, Option<String>, Option<u32>, Option<String>, Option<String>, Option<String>);

/// What `id` is a remix of, and its remixes here.
pub(super) fn links(db: &Connection, id: &str) -> rusqlite::Result<(Option<RemixLink>, Vec<ScriptSummary>)> {
    let stored: Option<StoredRemix> = db
        .query_row(
            "SELECT remix_server, remix_id, remix_version, remix_name, remix_author_id, remix_slug FROM scripts WHERE id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    let remix_of = match stored {
        Some((Some(server), Some(original), version, name, author_id, slug)) => {
            let here: Option<String> = if server.is_empty() {
                db.query_row("SELECT name FROM scripts WHERE id = ? AND hidden = 0", [&original], |r| r.get(0)).optional()?
            } else {
                None
            };
            Some(RemixLink {
                gone: server.is_empty() && here.is_none(),
                name: here.or(name).unwrap_or_default(),
                server,
                id: original,
                version,
                author_id,
                slug,
            })
        }
        _ => None,
    };
    let remixes = db
        .prepare(&format!(
            "SELECT {} FROM scripts WHERE remix_id = ? AND (remix_server IS NULL OR remix_server = '') AND hidden = 0
             ORDER BY created DESC LIMIT 50",
            super::SUMMARY_COLUMNS
        ))?
        .query_map([id], super::summary_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok((remix_of, remixes))
}
