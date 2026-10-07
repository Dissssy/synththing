//! `synththing serve`: a script library server (docs/SERVER.md).
//!
//! Everything lives in one data folder: `server.json` (its settings,
//! written with the defaults on first run), `server.key` (its signing
//! key, made on first run) and `library.db` (SQLite: scripts, their
//! versions, and a full-text index for search). Requests are served by a
//! few worker threads (`tiny_http`), sharing one database connection; at
//! the sizes a script library gets, that's plenty.
//!
//! What's here so far: uploads, anonymous or signed by the uploader's key
//! (`"mode": "signed"` takes only signed ones); a signed upload with a
//! slug its author used before is a new version of that script, and only
//! its author can delete it. Listing and search, details, user pages, and
//! sources and receipts signed with the server's key.
//!
//! Encores, reports, bans and the admin endpoint are in `moderation`.
//!
//! Previews (`preview.rs`): after each upload, the new version's preview
//! is made by `synththing preview` in a process of its own, one at a time,
//! with a time limit (`preview_seconds`); a script that stalls keeps the
//! still of its first frame. They're kept in `previews/` in the data
//! folder (`<id>-<version>.png`, `-sheet.png`), made with the starter pack's
//! TimGM6mb soundfont (downloaded into `soundfonts/` the first time) unless
//! `preview_soundfont` says otherwise. At startup, the newest version of
//! every script without one is queued.

mod authority;
mod copyright;
mod deletion;
mod mail;

#[cfg(test)]
pub use mail::Mail;
mod moderation;
mod notifications;
mod remixes;
mod songs;
mod update;

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response};

use super::{
    check_upload, hex, key_id, receipt_message, request_message, sha256_hex, sign_hex, source_message, unhex,
    verify_hex, ApiError, Info, Listing, Preview, Receipt, ScriptDetails, ScriptSummary, Upload, UserInfo, VersionInfo,
    KEY_HEADER, MAX_SOURCE_BYTES, NONCE_HEADER, PAGE_SIZE, REQUEST_WINDOW_SECONDS, SHA256_HEADER, SIGNATURE_HEADER,
    TIME_HEADER, VERSION_HEADER,
};

/// A server's settings (`server.json` in its data folder).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Shown in the app's server list.
    pub name: String,
    /// Address and port to listen on. Behind a reverse proxy (Caddy,
    /// nginx), keep it on 127.0.0.1.
    pub bind: String,
    /// Requests come through a reverse proxy: the client's address is the
    /// first one in `X-Forwarded-For`, not the connection's.
    pub behind_proxy: bool,
    /// `"open"` (unsigned uploads allowed, anonymously) or `"signed"`
    /// (only signed ones).
    pub mode: String,
    /// What uploads are shared under (an SPDX name).
    pub license: String,
    pub rules: String,
    /// Where reports and takedown requests go.
    pub contact: String,
    /// Uploads one address may make in a day.
    pub uploads_per_day: usize,
    /// Keys (hex) whose uploads aren't limited: the official publisher's,
    /// by default, for the bundled scripts CI publishes.
    pub unlimited_keys: Vec<String>,
    /// The publisher whose scripts are official here, as first published
    /// (the official one's by default): it's followed through its
    /// rotations, its current key isn't limited, and apps hear of it.
    pub publisher: String,
    /// Worker threads.
    pub threads: usize,
    /// Make previews of uploads.
    pub previews: bool,
    /// The soundfont previews play the starter songs with; none for the
    /// starter pack's TimGM6mb (downloaded the first time).
    pub preview_soundfont: Option<PathBuf>,
    /// How long making one preview may take before it's stopped (the
    /// script keeps the still of its first frame).
    pub preview_seconds: u64,
    /// This server is an identity authority: it issues rotations, and with
    /// `mail`, attaches emails and recovers identities by them.
    pub authority: bool,
    /// Its address as people reach it (for links in emails), e.g.
    /// `https://synththing.p51.nl`.
    pub public_url: String,
    /// The authorities whose rotations it takes (keys, hex): the official
    /// server's, by default.
    pub authorities: Vec<String>,
    /// Sending email (an authority's codes), through SendGrid.
    pub mail: Option<MailConfig>,
    /// On an authority with mail: uploads a day for a key without an email
    /// attached (or an anonymous upload), instead of `uploads_per_day`.
    pub unverified_uploads_per_day: usize,
    /// Documents to offer (an EULA, a privacy policy): each a title, a
    /// Phosphor icon's name and a Markdown file (in the data folder unless
    /// it's a full path). The app has a button for each.
    pub documents: Vec<DocumentConfig>,
    /// Refuse uploads that don't run here: they don't compile, or error in
    /// their first seconds (`synththing preview --check`).
    pub check_uploads: bool,
    /// Take songs (MIDI files) too.
    pub songs: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "synththing library".into(),
            bind: "127.0.0.1:7381".into(),
            behind_proxy: false,
            mode: "open".into(),
            license: "CC-BY-4.0".into(),
            rules: "Scripts here are shared under CC BY 4.0: anyone may use, change and share them, crediting \
                    the original (remixes link back to it). Nothing hateful, nothing meant to cause harm."
                .into(),
            contact: String::new(),
            uploads_per_day: 10,
            unlimited_keys: vec![super::OFFICIAL_PUBLISHER.to_string()],
            publisher: super::OFFICIAL_PUBLISHER.to_string(),
            threads: 4,
            previews: true,
            preview_soundfont: None,
            preview_seconds: 120,
            check_uploads: true,
            authority: false,
            public_url: String::new(),
            authorities: vec![super::rotation::OFFICIAL_AUTHORITY.to_string()],
            mail: None,
            unverified_uploads_per_day: 3,
            documents: Vec::new(),
            songs: false,
        }
    }
}

/// A document a server offers (`server.json`'s `documents`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DocumentConfig {
    pub title: String,
    pub icon: String,
    pub file: String,
}

impl DocumentConfig {
    fn slug(&self) -> String {
        super::slugify(&self.title)
    }
}

/// `GET /api/v1/documents/{slug}`: one of the documents, as Markdown.
fn document(state: &State, slug: &str) -> Reply {
    let Some(document) = state.config.documents.iter().find(|d| d.slug() == slug) else {
        return error(404, "no such document");
    };
    match std::fs::read_to_string(state.data.join(&document.file)) {
        Ok(text) => Response::from_data(text.into_bytes())
            .with_header(header("Content-Type", "text/markdown; charset=utf-8"))
            .with_header(header("Cache-Control", "public, max-age=300")),
        Err(e) => {
            println!("couldn't read the document {}: {e}", document.file);
            error(404, "that document is missing")
        }
    }
}

/// Email settings (`server.json`'s `mail`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MailConfig {
    /// The address emails come from (one SendGrid lets you send as).
    pub from: String,
    pub from_name: String,
    /// The file the SendGrid API key is in (in the data folder unless
    /// it's a full path).
    pub api_key_file: String,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self { from: String::new(), from_name: "synththing".into(), api_key_file: "sendgrid.key".into() }
    }
}

/// The secret addresses are hashed with: `email.pepper`, made on first run.
fn load_pepper(path: &Path) -> Result<Vec<u8>, String> {
    if let Ok(text) = std::fs::read_to_string(path) {
        return unhex(text.trim()).filter(|b| b.len() >= 16).ok_or_else(|| format!("{} isn't valid", path.display()));
    }
    let mut pepper = [0u8; 32];
    getrandom::fill(&mut pepper).map_err(|e| format!("couldn't make {}: {e}", path.display()))?;
    std::fs::write(path, hex(&pepper)).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(pepper.to_vec())
}

/// The folder a server keeps its data in when none is given.
pub fn default_data_dir() -> PathBuf {
    crate::config::config_dir().map(|d| d.join("server")).unwrap_or_else(|_| PathBuf::from("synththing-server"))
}

/// Run a server until it's stopped (Ctrl+C), printing what it's doing.
pub fn run(data: &Path, bind: Option<String>) -> Result<(), String> {
    let running = start(data, bind)?;
    println!(
        "synththing {} serving the library in {} at http://{} (key {})",
        env!("CARGO_PKG_VERSION"),
        data.display(),
        running.addr,
        super::key_id(&running.state.key_hex)
    );
    for thread in running.threads {
        let _ = thread.join();
    }
    Ok(())
}

/// A running server.
pub struct Running {
    pub addr: SocketAddr,
    #[cfg_attr(not(test), expect(dead_code))] // (stopping is for tests so far)
    http: Arc<tiny_http::Server>,
    threads: Vec<JoinHandle<()>>,
    state: Arc<State>,
}

impl Running {
    /// Purge deletions as if 30 days had gone by (tests).
    #[cfg(test)]
    pub fn purge_as_if_later(&self) {
        deletion::purge_as_of(&self.state, i64::MAX);
    }

    /// The emails it's "sent" (tests).
    #[cfg(test)]
    pub fn outbox(&self) -> Vec<mail::Mail> {
        self.state.mailer.outbox()
    }

    /// Stop it, waiting for requests under way.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn stop(self) {
        self.http.unblock();
        for _ in 1..self.threads.len() {
            self.http.unblock();
        }
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

/// Start a server from the data folder (made if it isn't there), on its
/// own threads.
pub fn start(data: &Path, bind: Option<String>) -> Result<Running, String> {
    std::fs::create_dir_all(data).map_err(|e| format!("couldn't make {}: {e}", data.display()))?;
    let config = load_config(&data.join("server.json"))?;
    let key = load_key(&data.join("server.key"))?;
    let db = open_db(&data.join("library.db")).map_err(|e| format!("couldn't open the database: {e}"))?;
    let key_hex = hex(&key.verifying_key().to_bytes());
    let bind = bind.unwrap_or_else(|| config.bind.clone());
    // (An update run by hand starts the new version as the old one stops:
    // the port takes a moment to come free.)
    let mut tries = 0;
    let http = loop {
        match tiny_http::Server::http(&bind) {
            Ok(http) => break http,
            Err(_) if tries < 40 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(format!("couldn't listen on {bind}: {e}")),
        }
    };
    let addr = http.server_addr().to_ip().ok_or("not listening on an IP address")?;
    let http = Arc::new(http);
    let workers = config.threads.max(1);
    let (queue, jobs) = mpsc::channel();
    let previews = config.previews;
    let pepper = load_pepper(&data.join("email.pepper"))?;
    let mailer = mail::Mailer::new(config.mail.as_ref(), data)?;
    if config.authority {
        println!("an identity authority{}", if mailer.on() { ", sending email" } else { " (no email: rotations only)" });
    }
    let state = Arc::new(State {
        config,
        key,
        key_hex,
        data: data.to_path_buf(),
        db: Mutex::new(db),
        uploads: Mutex::default(),
        seen_signatures: Mutex::default(),
        limits: Mutex::default(),
        streams: Mutex::default(),
        open_streams: AtomicUsize::new(0),
        previews: Mutex::new(previews.then_some(queue)),
        pepper,
        mailer,
    });
    moderation::forget_old_addresses(&state);
    deletion::purge(&state);
    copyright::expire(&state);
    {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("housekeeping".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(24 * 60 * 60));
                moderation::forget_old_addresses(&state);
                deletion::purge(&state);
                notifications::forget_old(&state);
                copyright::expire(&state);
            })
            .map_err(|e| format!("couldn't start: {e}"))?;
    }
    if previews {
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("previews".into())
            .stack_size(16 << 20)
            .spawn(move || make_previews(&state, jobs))
            .map_err(|e| format!("couldn't start making previews: {e}"))?;
    }
    let threads = (0..workers)
        .map(|_| {
            let http = Arc::clone(&http);
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                while let Ok(request) = http.recv() {
                    handle(&state, request);
                }
            })
        })
        .collect();
    Ok(Running { addr, http, threads, state })
}

struct State {
    config: ServerConfig,
    key: SigningKey,
    key_hex: String,
    /// The data folder.
    data: PathBuf,
    db: Mutex<Connection>,
    /// Recent uploads by address, for the daily limit.
    uploads: Mutex<HashMap<String, Vec<Instant>>>,
    /// Signatures of signed requests in the last while: the same one
    /// again is a replay.
    seen_signatures: Mutex<HashMap<String, Instant>>,
    /// Recent requests by kind and address, for their limits (encores,
    /// reports).
    limits: Mutex<HashMap<(String, String), Vec<Instant>>>,
    /// The notification streams open, by key, and how many in all.
    streams: Mutex<notifications::Streams>,
    open_streams: AtomicUsize,
    /// Versions to make previews of (none if previews are off).
    previews: Mutex<Option<Sender<(String, u32)>>>,
    /// What email addresses are hashed with, and the mail sender.
    pepper: Vec<u8>,
    mailer: mail::Mailer,
}

impl State {
    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn queue_preview(&self, id: &str, version: u32) {
        if let Some(queue) = &*self.previews.lock().unwrap_or_else(|p| p.into_inner()) {
            let _ = queue.send((id.to_string(), version));
        }
    }

    fn previews_dir(&self) -> PathBuf {
        self.data.join("previews")
    }

    fn preview_file(&self, id: &str, version: u32, sheet: bool) -> PathBuf {
        self.previews_dir().join(format!("{id}-{version}{}.png", if sheet { "-sheet" } else { "" }))
    }
}

/// The previews thread: those never made (at startup), then each upload's.
fn make_previews(state: &State, jobs: Receiver<(String, u32)>) {
    let soundfont = match preview_soundfont(state) {
        Ok(soundfont) => soundfont,
        Err(e) => {
            println!("not making previews: {e}");
            return;
        }
    };
    let waiting: Vec<(String, u32)> = state
        .db()
        .prepare(
            "SELECT s.id, s.latest FROM scripts s JOIN versions v ON v.script_id = s.id AND v.version = s.latest
             WHERE v.preview IS NULL ORDER BY s.updated DESC",
        )
        .and_then(|mut statement| {
            statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    if !waiting.is_empty() {
        println!("making {} preview{} not made yet", waiting.len(), if waiting.len() == 1 { "" } else { "s" });
    }
    for (id, version) in waiting.into_iter().chain(jobs.iter()) {
        make_preview(state, &soundfont, &id, version);
    }
}

fn preview_soundfont(state: &State) -> Result<PathBuf, String> {
    if let Some(path) = &state.config.preview_soundfont {
        return if path.exists() { Ok(path.clone()) } else { Err(format!("{} isn't there", path.display())) };
    }
    #[cfg(test)]
    return Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/starter/soundfonts/TimGM6mb.sf2"));
    #[cfg(not(test))]
    {
        let soundfont = &crate::starter::SOUNDFONTS[1];
        let path = state.data.join("soundfonts").join(soundfont.file);
        crate::starter::fetch(soundfont, &path)
            .map_err(|e| format!("couldn't get the {} soundfont: {e:#}", soundfont.name))?;
        Ok(path)
    }
}

/// Make one version's preview (unless it has one), and note what came of it.
fn make_preview(state: &State, soundfont: &Path, id: &str, version: u32) {
    // (Songs have none: the app plays theirs.)
    let source: Option<String> = state
        .db()
        .query_row(
            "SELECT source FROM versions WHERE script_id = ? AND version = ? AND preview IS NULL",
            params![id, version],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(source) = source else { return };
    let work = state.previews_dir().join("work");
    let _ = std::fs::remove_dir_all(&work);
    let script = work.join(format!("{id}.lua"));
    if let Err(e) = std::fs::create_dir_all(&work).and_then(|()| std::fs::write(&script, source)) {
        println!("couldn't make previews: {e}");
        return;
    }
    let started = Instant::now();
    let limit = Duration::from_secs(state.config.preview_seconds.max(5));
    #[cfg(not(test))]
    let result = crate::preview::run_process(&script, soundfont, &work, limit, false);
    #[cfg(test)]
    let result = {
        let _ = limit;
        crate::preview::render(&script, soundfont, &work).map(|_| ())
    };
    let keep = |name: &str, sheet: bool| {
        let made = work.join(name);
        made.exists() && std::fs::rename(&made, state.preview_file(id, version, sheet)).is_ok()
    };
    let preview = match (keep(crate::preview::STILL, false), keep(crate::preview::SHEET, true)) {
        (true, true) => "animated",
        (true, false) => "still",
        _ => "none",
    };
    let _ = std::fs::remove_dir_all(&work);
    let db = state.db();
    let updated = db
        .execute("UPDATE versions SET preview = ? WHERE script_id = ? AND version = ?", params![preview, id, version])
        .unwrap_or(0);
    drop(db);
    // (Deleted meanwhile.)
    if updated == 0 {
        remove_previews(state, id);
    }
    let why = result.err().map(|e| format!(" ({e})")).unwrap_or_default();
    println!("preview of {id} v{version}: {preview} in {:.1} s{why}", started.elapsed().as_secs_f64());
}

/// How long checking an upload runs may take; one that takes longer is
/// slow, not broken, and taken.
const CHECK_LIMIT: Duration = Duration::from_secs(12);

/// Whether `source` runs (on this server's version of the app): `Err` with
/// its error if it doesn't. Anything else going wrong lets it through.
fn check_runs(state: &State, source: &str) -> Result<(), String> {
    let Ok(soundfont) = preview_soundfont(state) else { return Ok(()) };
    let mut nonce = [0u8; 6];
    let _ = getrandom::fill(&mut nonce);
    let work = state.data.join("checks").join(hex(&nonce));
    let script = work.join("upload.lua");
    if std::fs::create_dir_all(&work).and_then(|()| std::fs::write(&script, source)).is_err() {
        return Ok(());
    }
    #[cfg(not(test))]
    let result = crate::preview::run_process(&script, &soundfont, &work, CHECK_LIMIT, true);
    #[cfg(test)]
    let result = {
        let _ = CHECK_LIMIT;
        crate::preview::check(&script, &soundfont, &work)
    };
    let _ = std::fs::remove_dir_all(&work);
    match result {
        Err(e) if e.starts_with(crate::preview::SCRIPT_ERROR) => {
            Err(e.trim_start_matches(crate::preview::SCRIPT_ERROR).to_string())
        }
        _ => Ok(()),
    }
}

/// Every preview of a script (it's been deleted).
fn remove_previews(state: &State, id: &str) {
    let prefix = format!("{id}-");
    if let Ok(entries) = std::fs::read_dir(state.previews_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

pub(super) fn load_config(path: &Path) -> Result<ServerConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} isn't valid: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let config = ServerConfig::default();
            let text = serde_json::to_string_pretty(&config).unwrap_or_default();
            std::fs::write(path, text).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
            Ok(config)
        }
        Err(e) => Err(format!("couldn't read {}: {e}", path.display())),
    }
}

/// The server's signing key: its 32-byte secret in hex, made on first run
/// and readable only by its owner.
fn load_key(path: &Path) -> Result<SigningKey, String> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let bytes = unhex(text.trim()).and_then(|b| <[u8; 32]>::try_from(b).ok());
        return bytes.map(|b| SigningKey::from_bytes(&b)).ok_or_else(|| format!("{} isn't a key", path.display()));
    }
    let key = super::new_signing_key().map_err(|e| format!("couldn't make a key: {e}"))?;
    std::fs::write(path, hex(&key.to_bytes())).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

/// The database, its tables made or brought up to date.
fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        db.execute_batch(SCHEMA_V1)?;
    }
    if version < 2 {
        db.execute_batch(SCHEMA_V2)?;
    }
    if version < 3 {
        // Each version's oldest app, worked out for those already stored.
        db.execute_batch("ALTER TABLE versions ADD COLUMN min_app_version TEXT NOT NULL DEFAULT '0.1.1';")?;
        let stored: Vec<(String, u32, String)> = db
            .prepare("SELECT script_id, version, source FROM versions")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, number, source) in stored {
            db.execute(
                "UPDATE versions SET min_app_version = ? WHERE script_id = ? AND version = ?",
                params![super::min_app_version(&source), id, number],
            )?;
        }
        db.execute_batch("PRAGMA user_version = 3;")?;
    }
    if version < 4 {
        // Each version's preview: NULL until it's been made.
        db.execute_batch("BEGIN; ALTER TABLE versions ADD COLUMN preview TEXT; PRAGMA user_version = 4; COMMIT;")?;
    }
    if version < 5 {
        db.execute_batch(moderation::SCHEMA_V5)?;
    }
    if version < 6 {
        db.execute_batch(moderation::SCHEMA_V6)?;
    }
    if version < 7 {
        db.execute_batch(remixes::SCHEMA_V7)?;
    }
    if version < 8 {
        db.execute_batch(authority::SCHEMA_V8)?;
    }
    if version < 9 {
        db.execute_batch(deletion::SCHEMA_V9)?;
    }
    if version < 10 {
        db.execute_batch(notifications::SCHEMA_V10)?;
    }
    if version < 11 {
        db.execute_batch(copyright::SCHEMA_V11)?;
    }
    if version < 12 {
        db.execute_batch(songs::SCHEMA_V12)?;
    }
    remixes::fingerprint_stored(&db)?;
    Ok(db)
}

/// Signed uploads: who posted (the key's short ID, to look them up by),
/// and the slug each script has among its author's.
const SCHEMA_V2: &str = "
BEGIN;
ALTER TABLE scripts ADD COLUMN slug TEXT;
ALTER TABLE scripts ADD COLUMN author_id TEXT;
CREATE UNIQUE INDEX scripts_author_slug ON scripts(author_key, slug) WHERE author_key IS NOT NULL;
CREATE INDEX scripts_author_id ON scripts(author_id);
PRAGMA user_version = 2;
COMMIT;
";

const SCHEMA_V1: &str = "
BEGIN;
CREATE TABLE scripts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    category TEXT NOT NULL,
    tags TEXT NOT NULL,             -- comma-separated
    author_name TEXT NOT NULL,
    author_key TEXT,                -- hex; none for an anonymous upload
    latest INTEGER NOT NULL,
    encores INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0,
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL
);
CREATE TABLE versions (
    script_id TEXT NOT NULL REFERENCES scripts(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    source TEXT NOT NULL,
    app_version TEXT NOT NULL,
    uploader_ip TEXT,
    created INTEGER NOT NULL,
    PRIMARY KEY (script_id, version)
);
CREATE INDEX versions_sha256 ON versions(sha256);
CREATE VIRTUAL TABLE scripts_fts USING fts5(name, description, tags, author_name, content='scripts', content_rowid='rowid');
CREATE TRIGGER scripts_ai AFTER INSERT ON scripts BEGIN
    INSERT INTO scripts_fts(rowid, name, description, tags, author_name)
        VALUES (new.rowid, new.name, new.description, new.tags, new.author_name);
END;
CREATE TRIGGER scripts_ad AFTER DELETE ON scripts BEGIN
    INSERT INTO scripts_fts(scripts_fts, rowid, name, description, tags, author_name)
        VALUES ('delete', old.rowid, old.name, old.description, old.tags, old.author_name);
END;
CREATE TRIGGER scripts_au AFTER UPDATE ON scripts BEGIN
    INSERT INTO scripts_fts(scripts_fts, rowid, name, description, tags, author_name)
        VALUES ('delete', old.rowid, old.name, old.description, old.tags, old.author_name);
    INSERT INTO scripts_fts(rowid, name, description, tags, author_name)
        VALUES (new.rowid, new.name, new.description, new.tags, new.author_name);
END;
PRAGMA user_version = 1;
COMMIT;
";

type Reply = Response<Cursor<Vec<u8>>>;

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("plain ASCII header")
}

fn json<T: Serialize>(status: u16, body: &T) -> Reply {
    Response::from_data(serde_json::to_vec(body).unwrap_or_default())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
}

fn error(status: u16, message: impl Into<String>) -> Reply {
    json(status, &ApiError { error: message.into(), retry_after: None })
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

fn handle(state: &Arc<State>, mut request: Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let query = parse_query(query);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    // (A stream keeps its connection, on a thread of its own.)
    if method == Method::Get && parts == ["api", "v1", "notifications", "stream"] {
        notifications::stream(state, request);
        return;
    }
    let reply = match (&method, parts.as_slice()) {
        (Method::Post, ["api", "v1", "notifications"]) => notifications::list(state, &mut request),
        (Method::Post, ["api", "v1", "notices"]) => {
            let ip = client_ip(state, &request);
            copyright::file(state, &mut request, &ip)
        }
        (Method::Post, ["api", "v1", "notices", id]) => match id.parse() {
            Ok(id) => copyright::view(state, &mut request, id),
            Err(_) => error(404, "no such notice"),
        },
        (Method::Post, ["api", "v1", "notices", id, "dispute"]) => match id.parse() {
            Ok(id) => copyright::dispute(state, &mut request, id),
            Err(_) => error(404, "no such notice"),
        },
        (Method::Post, ["api", "v1", "notices", id, "accept"]) => match id.parse() {
            Ok(id) => copyright::accept(state, &mut request, id),
            Err(_) => error(404, "no such notice"),
        },
        (Method::Post, ["api", "v1", "notifications", "mark"]) => notifications::mark(state, &mut request),
        (Method::Get, ["api", "v1", "info"]) => json(200, &info(state)),
        (Method::Get, ["api", "v1", "documents", slug]) => document(state, slug),
        (Method::Get, ["api", "v1", "scripts"]) => list(state, &query),
        (Method::Get, ["api", "v1", "scripts", id]) => details(state, id, &query),
        (Method::Get, ["api", "v1", "scripts", id, "source"]) => source(state, id, &query),
        (Method::Get, ["api", "v1", "scripts", id, "preview.png"]) => preview(state, id, &query, false),
        (Method::Get, ["api", "v1", "scripts", id, "preview-sheet.png"]) => preview(state, id, &query, true),
        (Method::Post, ["api", "v1", "songs"]) => {
            let ip = client_ip(state, &request);
            songs::upload(state, &mut request, &ip)
        }
        (Method::Get, ["api", "v1", "songs", id, "file"]) => {
            songs::file(state, id, query.get("version").and_then(|v| v.parse().ok()), !query.contains_key("preview"))
        }
        (Method::Get, ["api", "v1", "users", id]) => user(state, id),
        (Method::Get, ["api", "v1", "users", id, "scripts", slug]) => by_slug(state, id, slug),
        (Method::Post, ["api", "v1", "scripts"]) => {
            let ip = client_ip(state, &request);
            upload(state, &mut request, &ip)
        }
        (Method::Delete, ["api", "v1", "scripts", id]) => {
            let id = id.to_string();
            delete(state, &mut request, &id)
        }
        (Method::Post | Method::Delete, ["api", "v1", "scripts", id, "encore"]) => {
            let (id, ip) = (id.to_string(), client_ip(state, &request));
            moderation::encore(state, &mut request, &id, method == Method::Post, &ip)
        }
        (Method::Post, ["api", "v1", "scripts", id, "report"]) => {
            let (id, ip) = (id.to_string(), client_ip(state, &request));
            moderation::report(state, &mut request, &id, &ip)
        }
        (Method::Post, ["api", "v1", "admin"]) => moderation::admin(state, &mut request),
        (Method::Post, ["api", "v1", "identity", "rotate"]) => authority::rotate(state, &mut request),
        (Method::Post, ["api", "v1", "identity", "apply"]) => {
            let ip = client_ip(state, &request);
            authority::apply(state, &mut request, &ip)
        }
        (Method::Post, ["api", "v1", "identity", "status"]) => authority::status(state, &mut request),
        (Method::Get, ["api", "v1", "identity", "rotations"]) => {
            authority::lineage(state, query.get("key").map(String::as_str).unwrap_or_default())
        }
        (Method::Post, ["api", "v1", "identity", "email"]) => {
            let ip = client_ip(state, &request);
            authority::email(state, &mut request, &ip)
        }
        (Method::Post, ["api", "v1", "identity", "email", "confirm"]) => authority::email_confirm(state, &mut request),
        (Method::Post, ["api", "v1", "identity", "recover"]) => {
            let ip = client_ip(state, &request);
            authority::recover(state, &mut request, &ip)
        }
        (Method::Post, ["api", "v1", "identity", "recover", "confirm"]) => authority::recover_confirm(state, &mut request),
        (Method::Post, ["api", "v1", "identity", "delete"]) => {
            let ip = client_ip(state, &request);
            deletion::delete(state, &mut request, &ip)
        }
        (Method::Post, ["api", "v1", "identity", "delete", "confirm"]) => deletion::confirm(state, &mut request),
        (Method::Post, ["api", "v1", "identity", "restore"]) => deletion::restore(state, &mut request),
        (Method::Get, ["api", "v1", "identity", "cancel"]) => {
            authority::cancel(state, query.get("token").map(String::as_str).unwrap_or_default())
        }
        (Method::Get, [""]) => Response::from_string(format!(
            "{}: a synththing script library. Add this address under Preferences > Library in the app.\n",
            state.config.name
        ))
        .with_header(header("Content-Type", "text/plain; charset=utf-8")),
        _ => error(404, "no such page"),
    };
    let _ = request.respond(reply);
}

fn client_ip(state: &State, request: &Request) -> String {
    if state.config.behind_proxy
        && let Some(forwarded) = request.headers().iter().find(|h| h.field.equiv("X-Forwarded-For"))
        && let Some(first) = forwarded.value.as_str().split(',').next()
    {
        return first.trim().to_string();
    }
    request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default()
}

fn info(state: &State) -> Info {
    let publisher_rotations = authority::chain_from(&state.db(), &state.config.publisher);
    let documents = state
        .config
        .documents
        .iter()
        .map(|d| super::ServerDocument { title: d.title.clone(), icon: d.icon.clone(), slug: d.slug() })
        .collect();
    Info {
        documents,
        publisher_rotations,
        name: state.config.name.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        key: state.key_hex.clone(),
        mode: state.config.mode.clone(),
        license: state.config.license.clone(),
        rules: state.config.rules.clone(),
        contact: state.config.contact.clone(),
        authority: state.config.authority,
        mail: state.config.authority && state.mailer.on(),
        songs: state.config.songs,
    }
}

/// `a=1&b=x%20y` to pairs (later ones win).
fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => match text.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(b) => {
                    out.push(b);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const SUMMARY_COLUMNS: &str =
    "id, name, description, category, tags, author_name, author_key, latest, encores, created, updated, slug, kind";

fn summary_from_row(row: &rusqlite::Row) -> rusqlite::Result<ScriptSummary> {
    let tags: String = row.get(4)?;
    let author_key: Option<String> = row.get(6)?;
    Ok(ScriptSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        category: row.get(3)?,
        tags: tags.split(',').filter(|t| !t.is_empty()).map(str::to_string).collect(),
        author_name: row.get(5)?,
        author_id: author_key.as_deref().map(super::key_id),
        version: row.get(7)?,
        encores: row.get::<_, i64>(8)?.max(0) as u64,
        created: row.get(9)?,
        updated: row.get(10)?,
        slug: row.get(11)?,
        kind: row.get(12)?,
        song: None,
    })
}

/// Songs' own details, for the songs among `summaries`.
fn add_song_info(db: &Connection, summaries: &mut [ScriptSummary]) {
    for summary in summaries.iter_mut().filter(|s| s.kind == "song") {
        summary.song = songs::info(db, &summary.id);
    }
}

/// A search typed by someone as an FTS5 query: each word a prefix, all of
/// them required. Quotes are dropped, so nothing typed is FTS syntax.
fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn list(state: &State, query: &HashMap<String, String>) -> Reply {
    let mut clauses = vec!["hidden = 0".to_string(), "kind = ?".to_string()];
    let kind = if query.get("kind").is_some_and(|k| k == "song") { "song" } else { "script" };
    let mut args: Vec<rusqlite::types::Value> = vec![kind.to_string().into()];
    if let Some(category) = query.get("category").filter(|c| !c.is_empty()) {
        clauses.push("category = ?".into());
        args.push(category.clone().into());
    }
    if let Some(author) = query.get("author").filter(|a| !a.is_empty()) {
        clauses.push("author_id = ?".into());
        args.push(author.clone().into());
    }
    if let Some(tag) = query.get("tag").filter(|t| !t.is_empty()) {
        clauses.push("(',' || tags || ',') LIKE ?".into());
        args.push(format!("%,{},%", tag.to_lowercase()).into());
    }
    if let Some(fts) = query.get("q").and_then(|q| fts_query(q)) {
        clauses.push("rowid IN (SELECT rowid FROM scripts_fts WHERE scripts_fts MATCH ?)".into());
        args.push(fts.into());
    }
    let order = match query.get("sort").map(String::as_str) {
        Some("top") => "encores DESC, updated DESC, rowid DESC",
        _ => "updated DESC, rowid DESC",
    };
    let page: u32 = query.get("page").and_then(|p| p.parse().ok()).unwrap_or(0);
    let filter = clauses.join(" AND ");
    let db = state.db();
    let total: rusqlite::Result<u64> = db.query_row(
        &format!("SELECT COUNT(*) FROM scripts WHERE {filter}"),
        rusqlite::params_from_iter(args.iter()),
        |r| r.get::<_, i64>(0).map(|n| n.max(0) as u64),
    );
    let scripts = db
        .prepare(&format!(
            "SELECT {SUMMARY_COLUMNS} FROM scripts WHERE {filter} ORDER BY {order} LIMIT {PAGE_SIZE} OFFSET {}",
            page as usize * PAGE_SIZE
        ))
        .and_then(|mut statement| {
            statement.query_map(rusqlite::params_from_iter(args.iter()), summary_from_row)?.collect::<rusqlite::Result<Vec<_>>>()
        });
    match (total, scripts) {
        (Ok(total), Ok(mut scripts)) => {
            add_song_info(&db, &mut scripts);
            for script in &mut scripts {
                if script.description.chars().count() > 200 {
                    script.description = script.description.chars().take(200).collect::<String>() + "...";
                }
            }
            json(200, &Listing { scripts, total, page })
        }
        (Err(e), _) | (_, Err(e)) => error(500, format!("database: {e}")),
    }
}

fn find_summary(db: &Connection, id: &str) -> rusqlite::Result<Option<ScriptSummary>> {
    let mut found = db
        .query_row(&format!("SELECT {SUMMARY_COLUMNS} FROM scripts WHERE id = ? AND hidden = 0"), [id], summary_from_row)
        .optional()?;
    add_song_info(db, found.as_mut_slice());
    Ok(found)
}

fn details(state: &State, id: &str, query: &HashMap<String, String>) -> Reply {
    let db = state.db();
    let summary = match find_summary(&db, id) {
        Ok(Some(summary)) => summary,
        Ok(None) => return error(404, "no such script"),
        Err(e) => return error(500, format!("database: {e}")),
    };
    let latest = summary.version;
    let versions = db
        .prepare(
            "SELECT version, sha256, app_version, created, min_app_version, preview FROM versions WHERE script_id = ?
             ORDER BY version",
        )
        .and_then(|mut statement| {
            statement
                .query_map([id], |r| {
                    Ok(VersionInfo {
                        version: r.get(0)?,
                        sha256: r.get(1)?,
                        app_version: r.get(2)?,
                        created: r.get(3)?,
                        min_app_version: r.get(4)?,
                        preview: match r.get::<_, Option<String>>(5)?.as_deref() {
                            Some("animated") => Preview::Animated,
                            Some("still") => Preview::Still,
                            Some(_) => Preview::None,
                            // Only the newest version's is ever made.
                            None if latest == r.get::<_, u32>(0)? && state.config.previews => Preview::Pending,
                            None => Preview::None,
                        },
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        });
    match versions {
        Ok(versions) => {
            let encored = query.get("viewer").map(|key| moderation::has_encored(&db, id, key));
            match remixes::links(&db, id) {
                Ok((remix_of, remixes)) => json(200, &ScriptDetails { summary, versions, encored, remix_of, remixes }),
                Err(e) => error(500, format!("database: {e}")),
            }
        }
        Err(e) => error(500, format!("database: {e}")),
    }
}

fn source(state: &State, id: &str, query: &HashMap<String, String>) -> Reply {
    let db = state.db();
    let summary = match find_summary(&db, id) {
        Ok(Some(summary)) if summary.kind == "script" => summary,
        Ok(Some(_)) => return error(404, "that's a song, not a script: this app doesn't know songs yet"),
        Ok(None) => return error(404, "no such script"),
        Err(e) => return error(500, format!("database: {e}")),
    };
    let version: u32 = query.get("version").and_then(|v| v.parse().ok()).unwrap_or(summary.version);
    let found: rusqlite::Result<Option<(String, String)>> = db
        .query_row("SELECT source, sha256 FROM versions WHERE script_id = ? AND version = ?", params![id, version], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional();
    match found {
        Ok(Some((source, sha256))) => {
            notifications::downloaded(state, &db, id);
            let signature = sign_hex(&state.key, &source_message(&state.key_hex, id, version, &sha256));
            Response::from_data(source.into_bytes())
                .with_header(header("Content-Type", "text/plain; charset=utf-8"))
                .with_header(header(VERSION_HEADER, &version.to_string()))
                .with_header(header(SHA256_HEADER, &sha256))
                .with_header(header(SIGNATURE_HEADER, &signature))
        }
        Ok(None) => error(404, "no such version"),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `GET /api/v1/scripts/{id}/preview.png` (or `preview-sheet.png`):
/// `?version=`'s preview, or the newest earlier version's that has one;
/// without a version, the newest's.
fn preview(state: &State, id: &str, query: &HashMap<String, String>, sheet: bool) -> Reply {
    let db = state.db();
    let summary = match find_summary(&db, id) {
        Ok(Some(summary)) => summary,
        Ok(None) => return error(404, "no such script"),
        Err(e) => return error(500, format!("database: {e}")),
    };
    let upto: u32 = query.get("version").and_then(|v| v.parse().ok()).unwrap_or(summary.version);
    let kinds = if sheet { "('animated')" } else { "('still', 'animated')" };
    let found: rusqlite::Result<Option<u32>> = db
        .query_row(
            &format!(
                "SELECT version FROM versions WHERE script_id = ? AND version <= ? AND preview IN {kinds}
                 ORDER BY version DESC LIMIT 1"
            ),
            params![id, upto],
            |r| r.get(0),
        )
        .optional();
    drop(db);
    match found {
        Ok(Some(version)) => match std::fs::read(state.preview_file(id, version, sheet)) {
            Ok(bytes) => Response::from_data(bytes)
                .with_header(header("Content-Type", "image/png"))
                .with_header(header("Cache-Control", "public, max-age=3600"))
                .with_header(header(VERSION_HEADER, &version.to_string())),
            Err(_) => error(404, "its preview is missing"),
        },
        Ok(None) => error(404, "no preview yet"),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// Read a request's body, up to `limit` bytes.
fn read_body(request: &mut Request, limit: usize) -> Result<Vec<u8>, Reply> {
    read_body_or(request, limit, &format!("too big: scripts can be up to {} KB", MAX_SOURCE_BYTES / 1024))
}

/// As `read_body`, saying `too_big` when it is.
fn read_body_or(request: &mut Request, limit: usize, too_big: &str) -> Result<Vec<u8>, Reply> {
    let mut body = Vec::new();
    if request.as_reader().take(limit as u64 + 1).read_to_end(&mut body).is_err() {
        return Err(error(400, "couldn't read the request"));
    }
    if body.len() > limit {
        return Err(error(413, too_big));
    }
    Ok(body)
}

/// The signer of a signed request (their public key, hex), `None` for an
/// unsigned one, or why a signed one isn't accepted.
/// The signer of a signed request (as `signer_any`), unless their
/// identity's data is being deleted here: then only restoring it works.
fn signer(state: &State, request: &Request, method: &str, path: &str, body: &[u8]) -> Result<Option<String>, Reply> {
    let key = signer_any(state, request, method, path, body)?;
    if let Some(key) = &key
        && let Some(until) = deletion::pending(&state.db(), key)
    {
        let left = crate::song_info::format_length((until - now()).max(0) as f64);
        return Err(error(
            403,
            format!("your identity's data here is being deleted: restore it first (Preferences > Library) if you want it back (another {left} to change your mind)"),
        ));
    }
    Ok(key)
}

fn signer_any(state: &State, request: &Request, method: &str, path: &str, body: &[u8]) -> Result<Option<String>, Reply> {
    let get = |name: &'static str| {
        request.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().trim().to_string())
    };
    let (Some(key), Some(time), Some(signature)) = (get(KEY_HEADER), get(TIME_HEADER), get(SIGNATURE_HEADER)) else {
        return Ok(None);
    };
    let nonce = get(NONCE_HEADER).ok_or_else(|| error(400, "a signed request needs a nonce"))?;
    let time: i64 = time.parse().map_err(|_| error(400, "a signed request's time isn't a number"))?;
    if (now() - time).abs() > REQUEST_WINDOW_SECONDS {
        return Err(error(401, "the request's time is too far from the server's: is the computer's clock right?"));
    }
    if !verify_hex(&key, &request_message(&state.key_hex, method, path, time, &nonce, body), &signature) {
        return Err(error(401, "the request's signature doesn't match its key"));
    }
    if authority::is_retired(&state.db(), &key) {
        return Err(error(
            401,
            "this key was replaced by a newer one (a rotation or a recovery): the app with the new key can carry on",
        ));
    }
    let mut seen = state.seen_signatures.lock().unwrap_or_else(|p| p.into_inner());
    let window = Duration::from_secs(REQUEST_WINDOW_SECONDS as u64 * 2);
    seen.retain(|_, at| at.elapsed() < window);
    if seen.insert(signature, Instant::now()).is_some() {
        return Err(error(409, "that request was already made"));
    }
    Ok(Some(key))
}

fn signed_receipt(state: &State, id: String, version: u32, sha256: String) -> Receipt {
    let mut receipt = Receipt { id, version, sha256, time: now(), signature: String::new() };
    receipt.signature = sign_hex(&state.key, &receipt_message(&state.key_hex, &receipt));
    receipt
}

fn upload(state: &State, request: &mut Request, ip: &str) -> Reply {
    let body = match read_body(request, MAX_SOURCE_BYTES + 64 * 1024) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let author = match signer(state, request, "POST", "/api/v1/scripts", &body) {
        Ok(author) => author,
        Err(reply) => return reply,
    };
    if author.is_none() && state.config.mode != "open" {
        return error(403, "this server only takes signed uploads");
    }
    if let Err(reply) = moderation::refuse_banned(state, author.as_deref(), ip) {
        return reply;
    }
    let mut upload: Upload = match serde_json::from_slice(&body) {
        Ok(upload) => upload,
        Err(e) => return error(400, format!("not an upload: {e}")),
    };
    upload.name = upload.name.trim().to_string();
    upload.author_name = upload.author_name.trim().to_string();
    upload.tags = super::parse_tags(&upload.tags.join(","));
    if let Err(problem) = check_upload(&upload) {
        return error(400, problem);
    }
    // Slugs belong to signed uploads: an anonymous one can't be updated.
    let slug = match (&author, upload.slug.take()) {
        (Some(_), Some(slug)) => Some(slug),
        (Some(_), None) => return error(400, "a signed upload needs a slug"),
        (None, _) => None,
    };

    let sha256 = sha256_hex(upload.source.as_bytes());
    // It has to run (unless this very source was taken before).
    let known = state
        .db()
        .query_row("SELECT 1 FROM versions WHERE sha256 = ? LIMIT 1", [&sha256], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some();
    if state.config.check_uploads
        && !known
        && let Err(problem) = check_runs(state, &upload.source)
    {
        return error(400, format!("it doesn't run on synththing {}: {problem}", env!("CARGO_PKG_VERSION")));
    }
    let fingerprint = super::fingerprint::Fingerprint::of(&upload.source);
    let created = now();
    // The higher of the server's reckoning and the publisher's (a newer
    // app knows functions this server doesn't; claiming too high only hides
    // it from older apps).
    let mut min_app_version = super::min_app_version(&upload.source);
    if let Some(claimed) = &upload.min_app_version
        && !super::runs_on(claimed, &min_app_version)
    {
        min_app_version = claimed.clone();
    }
    let mut db = state.db();
    // A slug its author has used before: a new version of that script.
    let existing: rusqlite::Result<Option<(String, u32)>> = match (&author, &slug) {
        (Some(key), Some(slug)) => db
            .query_row("SELECT id, latest FROM scripts WHERE author_key = ? AND slug = ?", params![key, slug], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional(),
        _ => Ok(None),
    };
    let existing = match existing {
        Ok(existing) => existing,
        Err(e) => return error(500, format!("database: {e}")),
    };
    if let Some((id, latest)) = &existing {
        // The same source as the newest version: nothing to add.
        let same: rusqlite::Result<Option<String>> = db
            .query_row("SELECT sha256 FROM versions WHERE script_id = ? AND version = ?", params![id, latest], |r| r.get(0))
            .optional();
        if let Ok(Some(newest)) = same
            && newest == sha256
        {
            return json(200, &signed_receipt(state, id.clone(), *latest, sha256));
        }
    }
    // What it's a remix of; a copy of someone else's script it doesn't
    // credit that way isn't taken.
    let remix = match upload.remix_of.as_ref().map(|r| remixes::resolve(&db, r)).transpose() {
        Ok(remix) => remix,
        Err(e) => return error(500, format!("database: {e}")),
    };
    let same = existing.as_ref().map(|(id, _)| id.as_str());
    let asked_and_found = upload.remix_of.as_ref().zip(remix.as_ref());
    let publisher = authority::chain_from(&db, &state.config.publisher)
        .last()
        .map_or_else(|| state.config.publisher.clone(), |r| r.new.clone());
    match copyright::blocked(&db, &fingerprint) {
        Ok(true) if crate::library::fingerprint::tokens(&upload.source).len() >= 40 => {
            return error(400, "it's the same as a script taken down after a copyright notice");
        }
        Ok(_) => {}
        Err(e) => return error(500, format!("database: {e}")),
    }
    match remixes::refuse_copy(&db, &upload.source, &fingerprint, author.as_deref(), same, asked_and_found, &publisher) {
        Ok(Some(why)) => return error(400, why),
        Ok(None) => {}
        Err(e) => return error(500, format!("database: {e}")),
    }
    let limited = match upload_allowance(state, &db, author.as_deref(), ip) {
        Ok(limited) => limited,
        Err(reply) => return reply,
    };
    let id = match &existing {
        Some((id, _)) => id.clone(),
        None => loop {
            let mut bytes = [0u8; 5];
            if getrandom::fill(&mut bytes).is_err() {
                return error(500, "couldn't make an ID");
            }
            let id = super::base32(&bytes);
            match db.query_row("SELECT 1 FROM scripts WHERE id = ?", [&id], |_| Ok(())).optional() {
                Ok(None) => break id,
                Ok(Some(())) => continue,
                Err(e) => return error(500, format!("database: {e}")),
            }
        },
    };
    let version = existing.as_ref().map_or(1, |(_, latest)| latest + 1);
    let author_id = author.as_deref().map(key_id);
    let stored = db.transaction().and_then(|tx| {
        if existing.is_some() {
            tx.execute(
                "UPDATE scripts SET name = ?, description = ?, category = ?, tags = ?, author_name = ?, latest = ?,
                 updated = ? WHERE id = ?",
                params![
                    upload.name,
                    upload.description,
                    upload.category,
                    upload.tags.join(","),
                    upload.author_name,
                    version,
                    created,
                    id
                ],
            )?;
            if let Some(remix) = &remix {
                tx.execute(
                    "UPDATE scripts SET remix_server = ?, remix_id = ?, remix_version = ?, remix_name = ?, remix_author_id = ?,
                     remix_slug = ? WHERE id = ?",
                    params![remix.server, remix.id, remix.version, remix.name, remix.author_id, remix.slug, id],
                )?;
            }
        } else {
            tx.execute(
                "INSERT INTO scripts (id, name, description, category, tags, author_name, author_key, author_id, slug,
                 latest, created, updated, remix_server, remix_id, remix_version, remix_name, remix_author_id, remix_slug)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    id,
                    upload.name,
                    upload.description,
                    upload.category,
                    upload.tags.join(","),
                    upload.author_name,
                    author,
                    author_id,
                    slug,
                    created,
                    created,
                    remix.as_ref().map(|r| r.server.clone()),
                    remix.as_ref().map(|r| r.id.clone()),
                    remix.as_ref().and_then(|r| r.version),
                    remix.as_ref().map(|r| r.name.clone()),
                    remix.as_ref().and_then(|r| r.author_id.clone()),
                    remix.as_ref().and_then(|r| r.slug.clone()),
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO versions (script_id, version, sha256, source, app_version, uploader_ip, created, min_app_version,
             fingerprint) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![id, version, sha256, upload.source, upload.app_version, ip, created, min_app_version, fingerprint.to_hex()],
        )?;
        tx.commit()
    });
    if let Err(e) = stored {
        return error(500, format!("database: {e}"));
    }
    if limited {
        count_upload(state, ip);
    }
    // (A new remix here: its original's author hears of it.)
    if existing.is_none()
        && let Some(remix) = remix.as_ref().filter(|r| r.server.is_empty() && !r.id.is_empty())
    {
        let (by, remix_id, remix_name) = (upload.author_name.clone(), id.clone(), upload.name.clone());
        notifications::author_notify(state, &db, &remix.id, "remixed", |name| {
            serde_json::json!({ "script": remix.id, "name": name, "remix": remix_id, "remix_name": remix_name, "by": by })
        });
    }
    drop(db);
    state.queue_preview(&id, version);
    let who = author_id.map(|id| format!(" (#{id})")).unwrap_or_default();
    println!("uploaded {id} v{version} \"{}\" by {}{who} from {ip}", upload.name, upload.author_name);
    json(201, &signed_receipt(state, id, version, sha256))
}

/// The daily limit on uploads (scripts and songs alike), by address, for
/// anything new: whether this one counts (keys the server trusts aside: the
/// official publisher, by default), or why it's refused.
fn upload_allowance(state: &State, db: &Connection, author: Option<&str>, ip: &str) -> Result<bool, Reply> {
    let publisher = authority::current_key(db, &state.config.publisher);
    let limited = !author.is_some_and(|key| state.config.unlimited_keys.iter().any(|k| k == key) || key == publisher);
    if limited {
        let mut uploads = state.uploads.lock().unwrap_or_else(|p| p.into_inner());
        let recent = uploads.entry(ip.to_string()).or_default();
        let day = Duration::from_secs(24 * 60 * 60);
        recent.retain(|t| t.elapsed() < day);
        // (An authority with mail is stricter with keys it can't recover.)
        let verified = author.is_some_and(|key| authority::has_email(db, key));
        let per_day = if state.config.authority && state.mailer.on() && !verified {
            state.config.unverified_uploads_per_day.min(state.config.uploads_per_day)
        } else {
            state.config.uploads_per_day
        };
        if recent.len() >= per_day {
            let wait = recent.first().map(|t| day.saturating_sub(t.elapsed()).as_secs()).unwrap_or(0);
            return Err(json(
                429,
                &ApiError { error: "that's all the uploads for today from here".into(), retry_after: Some(wait) },
            ));
        }
    }
    Ok(limited)
}

/// An upload `upload_allowance` said counts, taken.
fn count_upload(state: &State, ip: &str) {
    state.uploads.lock().unwrap_or_else(|p| p.into_inner()).entry(ip.to_string()).or_default().push(Instant::now());
}

/// `DELETE /api/v1/scripts/{id}`, signed by its author.
fn delete(state: &State, request: &mut Request, id: &str) -> Reply {
    let path = format!("/api/v1/scripts/{id}");
    let key = match signer(state, request, "DELETE", &path, &[]) {
        Ok(Some(key)) => key,
        Ok(None) => return error(401, "deleting a script needs its author's signature"),
        Err(reply) => return reply,
    };
    let db = state.db();
    let owner: rusqlite::Result<Option<Option<String>>> =
        db.query_row("SELECT author_key FROM scripts WHERE id = ?", [id], |r| r.get(0)).optional();
    match owner {
        Ok(None) => error(404, "no such script"),
        Ok(Some(owner)) if owner.as_deref() != Some(key.as_str()) => error(403, "only its author can delete it"),
        Ok(Some(_)) => match db.execute("DELETE FROM scripts WHERE id = ?", [id]) {
            Ok(_) => {
                remove_previews(state, id);
                println!("deleted {id} (by #{})", key_id(&key));
                json(200, &serde_json::json!({ "deleted": id }))
            }
            Err(e) => error(500, format!("database: {e}")),
        },
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `GET /api/v1/users/{id}/scripts/{slug}`: one of their scripts, by slug.
fn by_slug(state: &State, author_id: &str, slug: &str) -> Reply {
    let found = state
        .db()
        .query_row(
            &format!("SELECT {SUMMARY_COLUMNS} FROM scripts WHERE author_id = ? AND slug = ? AND hidden = 0"),
            params![author_id, slug],
            summary_from_row,
        )
        .optional();
    match found {
        Ok(Some(summary)) => json(200, &summary),
        Ok(None) => error(404, "no script with that slug"),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `GET /api/v1/users/{id}`: the names they've posted under, and how much.
fn user(state: &State, id: &str) -> Reply {
    let db = state.db();
    let names = db
        .prepare(
            "SELECT author_name FROM scripts WHERE author_id = ? AND hidden = 0 GROUP BY author_name ORDER BY COUNT(*) DESC",
        )
        .and_then(|mut statement| statement.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>());
    let totals: rusqlite::Result<(i64, i64)> = db.query_row(
        "SELECT COUNT(*), COALESCE(SUM(encores), 0) FROM scripts WHERE author_id = ? AND hidden = 0",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    );
    match (names, totals) {
        (Ok(names), Ok((scripts, encores))) if scripts > 0 => json(
            200,
            &UserInfo { id: id.to_string(), names, scripts: scripts as u64, encores: encores.max(0) as u64 },
        ),
        (Ok(_), Ok(_)) => error(404, "no uploads by that ID"),
        (Err(e), _) | (_, Err(e)) => error(500, format!("database: {e}")),
    }
}
