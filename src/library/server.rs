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
    check_upload, hex, key_id, receipt_message, request_message, sha256_hex, sign_hex, source_message, unhex,
    verify_hex, ApiError, Info, Listing, Receipt, ScriptDetails, ScriptSummary, Upload, UserInfo, VersionInfo,
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
            unlimited_keys: vec![super::OFFICIAL_PUBLISHER.to_string()],
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
    let state = Arc::new(State {
        config,
        key,
        key_hex,
        db: Mutex::new(db),
        uploads: Mutex::default(),
        seen_signatures: Mutex::default(),
    });
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
    /// Signatures of signed requests in the last while: the same one
    /// again is a replay.
    seen_signatures: Mutex<HashMap<String, Instant>>,
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
    "id, name, description, category, tags, author_name, author_key, latest, encores, created, updated, slug";

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
        .prepare(
            "SELECT version, sha256, app_version, created, min_app_version FROM versions WHERE script_id = ? ORDER BY version",
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
                    })
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

/// Read a request's body, up to `limit` bytes.
fn read_body(request: &mut Request, limit: usize) -> Result<Vec<u8>, Reply> {
    let mut body = Vec::new();
    if request.as_reader().take(limit as u64 + 1).read_to_end(&mut body).is_err() {
        return Err(error(400, "couldn't read the request"));
    }
    if body.len() > limit {
        return Err(error(413, format!("too big: scripts can be up to {} KB", MAX_SOURCE_BYTES / 1024)));
    }
    Ok(body)
}

/// The signer of a signed request (their public key, hex), `None` for an
/// unsigned one, or why a signed one isn't accepted.
fn signer(state: &State, request: &Request, method: &str, path: &str, body: &[u8]) -> Result<Option<String>, Reply> {
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
    // The daily limit, by address, for anything new (keys the server
    // trusts aside: the official publisher, by default).
    let limited = !author.as_ref().is_some_and(|key| state.config.unlimited_keys.contains(key));
    if limited {
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
        } else {
            tx.execute(
                "INSERT INTO scripts (id, name, description, category, tags, author_name, author_key, author_id, slug,
                 latest, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
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
                    created
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO versions (script_id, version, sha256, source, app_version, uploader_ip, created, min_app_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![id, version, sha256, upload.source, upload.app_version, ip, created, min_app_version],
        )?;
        tx.commit()
    });
    if let Err(e) = stored {
        return error(500, format!("database: {e}"));
    }
    if limited {
        state.uploads.lock().unwrap_or_else(|p| p.into_inner()).entry(ip.to_string()).or_default().push(Instant::now());
    }
    let who = author_id.map(|id| format!(" (#{id})")).unwrap_or_default();
    println!("uploaded {id} v{version} \"{}\" by {}{who} from {ip}", upload.name, upload.author_name);
    json(201, &signed_receipt(state, id, version, sha256))
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
