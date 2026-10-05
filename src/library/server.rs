//! `synththing serve`: a script library server (docs/SERVER.md).
//!
//! Everything lives in one data folder: `server.json` (its settings,
//! written with the defaults on first run), `server.key` (its signing
//! key, made on first run) and `library.db` (SQLite: scripts, their
//! versions, and a full-text index for search). Requests are served by a
//! few worker threads (`tiny_http`), sharing one database connection; at
//! the sizes a script library gets, that's plenty.
//!
//! What's here so far is the first step of the plan: open uploads
//! (anonymous, unsigned), listing and search, details, and sources signed
//! with the server's key.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response};

use super::{
    check_upload, hex, sha256_hex, sign_hex, source_message, unhex, ApiError, Info, Listing, Receipt,
    ScriptDetails, ScriptSummary, Upload, VersionInfo, MAX_SOURCE_BYTES, PAGE_SIZE, SHA256_HEADER,
    SIGNATURE_HEADER, VERSION_HEADER,
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
    /// `"open"` (unsigned uploads allowed, anonymously) or `"signed"`.
    pub mode: String,
    /// What uploads are shared under (an SPDX name).
    pub license: String,
    pub rules: String,
    /// Where reports and takedown requests go.
    pub contact: String,
    /// Uploads one address may make in a day.
    pub uploads_per_day: usize,
    /// Worker threads.
    pub threads: usize,
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
            threads: 4,
        }
    }
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
    let http = tiny_http::Server::http(&bind).map_err(|e| format!("couldn't listen on {bind}: {e}"))?;
    let addr = http.server_addr().to_ip().ok_or("not listening on an IP address")?;
    let http = Arc::new(http);
    let workers = config.threads.max(1);
    let state = Arc::new(State { config, key, key_hex, db: Mutex::new(db), uploads: Mutex::default() });
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
    db: Mutex<Connection>,
    /// Recent uploads by address, for the daily limit.
    uploads: Mutex<HashMap<String, Vec<Instant>>>,
}

impl State {
    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn load_config(path: &Path) -> Result<ServerConfig, String> {
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
    Ok(db)
}

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

fn handle(state: &State, mut request: Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let query = parse_query(query);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let reply = match (&method, parts.as_slice()) {
        (Method::Get, ["api", "v1", "info"]) => json(200, &info(state)),
        (Method::Get, ["api", "v1", "scripts"]) => list(state, &query),
        (Method::Get, ["api", "v1", "scripts", id]) => details(state, id),
        (Method::Get, ["api", "v1", "scripts", id, "source"]) => source(state, id, &query),
        (Method::Post, ["api", "v1", "scripts"]) => {
            let ip = client_ip(state, &request);
            upload(state, &mut request, &ip)
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
    Info {
        name: state.config.name.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        key: state.key_hex.clone(),
        mode: state.config.mode.clone(),
        license: state.config.license.clone(),
        rules: state.config.rules.clone(),
        contact: state.config.contact.clone(),
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
    "id, name, description, category, tags, author_name, author_key, latest, encores, created, updated";

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
    })
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
    let mut clauses = vec!["hidden = 0".to_string()];
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(category) = query.get("category").filter(|c| !c.is_empty()) {
        clauses.push("category = ?".into());
        args.push(category.clone().into());
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
    db.query_row(&format!("SELECT {SUMMARY_COLUMNS} FROM scripts WHERE id = ? AND hidden = 0"), [id], summary_from_row)
        .optional()
}

fn details(state: &State, id: &str) -> Reply {
    let db = state.db();
    let summary = match find_summary(&db, id) {
        Ok(Some(summary)) => summary,
        Ok(None) => return error(404, "no such script"),
        Err(e) => return error(500, format!("database: {e}")),
    };
    let versions = db
        .prepare("SELECT version, sha256, app_version, created FROM versions WHERE script_id = ? ORDER BY version")
        .and_then(|mut statement| {
            statement
                .query_map([id], |r| {
                    Ok(VersionInfo { version: r.get(0)?, sha256: r.get(1)?, app_version: r.get(2)?, created: r.get(3)? })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        });
    match versions {
        Ok(versions) => json(200, &ScriptDetails { summary, versions }),
        Err(e) => error(500, format!("database: {e}")),
    }
}

fn source(state: &State, id: &str, query: &HashMap<String, String>) -> Reply {
    let db = state.db();
    let summary = match find_summary(&db, id) {
        Ok(Some(summary)) => summary,
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

fn upload(state: &State, request: &mut Request, ip: &str) -> Reply {
    if state.config.mode != "open" {
        return error(403, "this server only takes signed uploads, which this version of synththing can't make yet");
    }
    // The daily limit, by address.
    {
        let mut uploads = state.uploads.lock().unwrap_or_else(|p| p.into_inner());
        let recent = uploads.entry(ip.to_string()).or_default();
        let day = Duration::from_secs(24 * 60 * 60);
        recent.retain(|t| t.elapsed() < day);
        if recent.len() >= state.config.uploads_per_day {
            let wait = recent.first().map(|t| day.saturating_sub(t.elapsed()).as_secs()).unwrap_or(0);
            return json(
                429,
                &ApiError { error: "that's all the uploads for today from here".into(), retry_after: Some(wait) },
            );
        }
    }
    let mut body = Vec::new();
    let limit = (MAX_SOURCE_BYTES + 64 * 1024) as u64;
    if request.as_reader().take(limit + 1).read_to_end(&mut body).is_err() {
        return error(400, "couldn't read the upload");
    }
    if body.len() as u64 > limit {
        return error(413, format!("too big: scripts can be up to {} KB", MAX_SOURCE_BYTES / 1024));
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

    let sha256 = sha256_hex(upload.source.as_bytes());
    let created = now();
    let mut db = state.db();
    let id = loop {
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
    };
    let stored = db.transaction().and_then(|tx| {
        tx.execute(
            "INSERT INTO scripts (id, name, description, category, tags, author_name, author_key, latest, created, updated)
             VALUES (?, ?, ?, ?, ?, ?, NULL, 1, ?, ?)",
            params![id, upload.name, upload.description, upload.category, upload.tags.join(","), upload.author_name, created, created],
        )?;
        tx.execute(
            "INSERT INTO versions (script_id, version, sha256, source, app_version, uploader_ip, created) VALUES (?, 1, ?, ?, ?, ?, ?)",
            params![id, sha256, upload.source, upload.app_version, ip, created],
        )?;
        tx.commit()
    });
    if let Err(e) = stored {
        return error(500, format!("database: {e}"));
    }
    state.uploads.lock().unwrap_or_else(|p| p.into_inner()).entry(ip.to_string()).or_default().push(Instant::now());
    println!("uploaded {id} \"{}\" by {} from {ip}", upload.name, upload.author_name);
    json(201, &Receipt { id, version: 1, sha256 })
}
