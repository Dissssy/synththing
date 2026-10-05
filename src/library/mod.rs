//! The script library (see docs/SERVER.md): servers holding shared
//! visualizer scripts, and the app's side of talking to them.
//!
//! This module has what both sides agree on: the JSON the API sends and
//! takes, the categories, and how a server signs what it serves. `server`
//! is `synththing serve`; `client` is the app's requests.

pub mod admin;
pub mod client;
pub mod identity;
pub mod server;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The official server.
pub const OFFICIAL_URL: &str = "https://synththing.p51.nl";

/// The official server's address: `OFFICIAL_URL`, unless the
/// `SYNTHTHING_OFFICIAL_URL` environment variable says otherwise (for
/// trying things against a server of your own).
pub fn official_url() -> String {
    std::env::var("SYNTHTHING_OFFICIAL_URL")
        .map(|url| client::normalize_url(&url))
        .unwrap_or_else(|_| OFFICIAL_URL.to_string())
}

/// Where to reach a server recorded in a `.source.json`: the official one
/// is recorded as `OFFICIAL_URL` (so the files stay right whatever
/// `SYNTHTHING_OFFICIAL_URL` said when they were written), and reached at
/// `official_url()`.
pub fn resolve_server(url: &str) -> String {
    if url == OFFICIAL_URL { official_url() } else { url.to_string() }
}

/// The key the official scripts are published with (the ones bundled with
/// the app, published by CI from the repository: `publish_bundled`).
/// Scripts signed by it are marked official.
pub const OFFICIAL_PUBLISHER: &str = "3fc0018f1b43c6b4708b9ac49736be1835fe4c469647bd3b28cac9e4d5746d04";

/// The official publisher's ID.
pub fn official_id() -> String {
    key_id(OFFICIAL_PUBLISHER)
}

/// What a bundled script is on the library: its slug (from its file
/// name), name, kind and description.
fn bundled_upload(category: &str, file_name: &str, source: &str) -> Option<Upload> {
    // (A check of the script editor's features, not a script to share.)
    if file_name == "editor_test.lua" {
        return None;
    }
    let category = match category {
        "visualizers" => "visualizer",
        "games" => "game",
        "examples" => "example",
        _ => return None, // (templates are starting points, not scripts to share)
    };
    let stem = file_name.strip_suffix(".lua").unwrap_or(file_name);
    Some(Upload {
        name: stem.to_string(),
        description: leading_comment(source),
        category: category.to_string(),
        tags: Vec::new(),
        author_name: "synththing".to_string(),
        source: source.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        slug: Some(slugify(stem)),
        min_app_version: Some(min_app_version(source)),
    })
}

/// Globals `source` uses that this build doesn't know (not the app's, not
/// Lua's, not the script's own): most likely functions newer than this
/// build, so its `min_app_version` can't be trusted.
pub fn unknown_globals(source: &str) -> Vec<String> {
    let known = crate::lua_analysis::known_globals(crate::lua_completion::HOST_API.iter().map(|(n, _)| n.to_string()));
    let analysis = crate::lua_analysis::analyze(source, &known);
    let mut unknown: Vec<String> = analysis
        .refs
        .iter()
        .filter(|r| r.local.is_none() && !analysis.script_globals.contains(&r.name) && !known.contains(&r.name))
        .map(|r| r.name.clone())
        .collect();
    unknown.sort();
    unknown.dedup();
    unknown
}

/// The bundled scripts, as (category, file name, source): built into this
/// binary, or read from a folder laid out like `assets/visualizers`.
fn bundled_scripts(dir: Option<&std::path::Path>) -> Result<Vec<(String, String, String)>, String> {
    let Some(dir) = dir else {
        return Ok(crate::lua_visualizer::BUNDLED_SCRIPTS
            .iter()
            .map(|&(c, n, s)| (c.to_string(), n.to_string(), s.to_string()))
            .collect());
    };
    let mut scripts = Vec::new();
    for category in ["visualizers", "games", "templates", "examples"] {
        let Ok(entries) = std::fs::read_dir(dir.join(category)) else { continue };
        let mut files: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("lua"))
            .collect();
        files.sort();
        for path in files {
            let source = std::fs::read_to_string(&path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            scripts.push((category.to_string(), name, source));
        }
    }
    if scripts.is_empty() {
        return Err(format!("no scripts in {} (it should have visualizers/, games/, ...)", dir.display()));
    }
    Ok(scripts)
}

/// Mark the bundled visualizers and games in the scripts folder as
/// installed from the official server (unless they're marked already), so
/// Update and Restore original work on them like on anything installed:
/// a `.source.json` naming them by slug (their ID on the server isn't
/// known yet; an update check finds it), and their bundled source kept as
/// the original.
pub fn mark_bundled(dir: &std::path::Path) {
    for &(category, file_name, source) in crate::lua_visualizer::BUNDLED_SCRIPTS {
        if !matches!(category, "visualizers" | "games") {
            continue;
        }
        let path = dir.join(file_name);
        if !path.exists() || installed_path(&path).exists() {
            continue;
        }
        let Some(upload) = bundled_upload(category, file_name, source) else { continue };
        let installed = Installed {
            server: OFFICIAL_URL.to_string(),
            id: String::new(),
            version: 0,
            sha256: sha256_hex(source.as_bytes()),
            name: upload.name,
            slug: upload.slug,
            author_id: Some(official_id()),
            author_name: Some(upload.author_name),
            category: Some(upload.category),
            receipt: None,
        };
        if installed.write(&path).is_ok() {
            save_original(source);
        }
    }
}

/// `synththing publish-bundled`: publish every bundled script (not the
/// templates) to `server`, signed with the secret key in the
/// `SYNTHTHING_PUBLISH_KEY` environment variable, each by its slug: one
/// that's changed becomes a new version, an unchanged one is left as it
/// is. The scripts are the ones built into this binary, or with `dir`,
/// the ones in that folder (CI, between releases, with the newest release's
/// binary); then a script using something this binary doesn't know (a
/// function newer than it) is skipped, to be published by the release that
/// has it.
pub fn publish_bundled(server: &str, dir: Option<&std::path::Path>) -> Result<(), String> {
    let secret = std::env::var("SYNTHTHING_PUBLISH_KEY").map_err(|_| "SYNTHTHING_PUBLISH_KEY isn't set")?;
    let identity = identity::Identity::from_secret_hex(&secret).ok_or("SYNTHTHING_PUBLISH_KEY isn't a key")?;
    if identity.public() != OFFICIAL_PUBLISHER {
        println!("note: this isn't the official publisher key (it's #{})", identity.id());
    }
    let server = client::normalize_url(server);
    let key = client::info(&server)?.key;
    let mut failed = 0;
    for (category, file_name, source) in bundled_scripts(dir)? {
        let Some(upload) = bundled_upload(&category, &file_name, &source) else { continue };
        let unknown = unknown_globals(&source);
        if dir.is_some() && !unknown.is_empty() {
            println!("{file_name}: skipped: uses {} (newer than this build; the next release publishes it)", unknown.join(", "));
            continue;
        }
        match client::upload(&server, &key, &upload, Some(&identity)) {
            Ok(receipt) => println!("{file_name}: {} version {}", receipt.id, receipt.version),
            Err(e) => {
                println!("{file_name}: failed: {e}");
                failed += 1;
            }
        }
    }
    if failed > 0 { Err(format!("{failed} script(s) failed")) } else { Ok(()) }
}

/// The categories a script can be in, as the API names them.
pub const CATEGORIES: [&str; 4] = ["visualizer", "game", "toy", "example"];

/// A category as shown.
pub fn category_title(category: &str) -> &str {
    match category {
        "visualizer" => "Visualizer",
        "game" => "Game",
        "toy" => "Toy",
        "example" => "Example",
        other => other,
    }
}

/// The largest script a server takes.
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// Lengths of the text that comes with one.
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_DESCRIPTION_CHARS: usize = 4000;
pub const MAX_TAGS: usize = 8;
pub const MAX_TAG_CHARS: usize = 24;
/// Scripts in one page of a listing.
pub const PAGE_SIZE: usize = 30;

/// `GET /api/v1/info`: what a server is and what it asks of uploads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Info {
    pub name: String,
    /// The synththing version the server runs.
    pub version: String,
    /// The server's public key (hex), which signs what it serves.
    pub key: String,
    /// `"open"` or `"signed"`.
    pub mode: String,
    /// What uploads are shared under (an SPDX name, e.g. `CC-BY-4.0`).
    pub license: String,
    pub rules: String,
    pub contact: String,
}

/// One script in a listing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScriptSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    /// The name it was posted under.
    pub author_name: String,
    /// The poster's ID, for a signed upload (none for an anonymous one).
    pub author_id: Option<String>,
    /// Its name in its author's uploads (signed uploads only): another
    /// upload with it is a new version.
    #[serde(default)]
    pub slug: Option<String>,
    /// Its newest version (1 for a first upload).
    pub version: u32,
    pub encores: u64,
    /// Unix seconds.
    pub created: i64,
    pub updated: i64,
}

/// `GET /api/v1/scripts`: a page of scripts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Listing {
    pub scripts: Vec<ScriptSummary>,
    /// How many match in all.
    pub total: u64,
    pub page: u32,
}

/// One version of a script.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VersionInfo {
    pub version: u32,
    pub sha256: String,
    /// The app version it was published from.
    pub app_version: String,
    /// The oldest app it runs on (`min_app_version`).
    #[serde(default = "oldest_version")]
    pub min_app_version: String,
    pub created: i64,
    /// Its preview, as far as the server's got with it.
    #[serde(default)]
    pub preview: Preview,
}

/// A version's preview (`preview.rs`): made by the server after each
/// upload, an animation and a still (`/preview-sheet.png`,
/// `/preview.png`), or only a still.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preview {
    /// Waiting to be made.
    Pending,
    Still,
    Animated,
    /// None was made (an older version, a script that doesn't run, or a
    /// server that doesn't make them).
    #[default]
    #[serde(other)]
    None,
}

fn oldest_version() -> String {
    "0.1.1".into()
}

impl ScriptDetails {
    /// The newest version an app at `app` can run, if any.
    pub fn newest_for(&self, app: &str) -> Option<&VersionInfo> {
        self.versions.iter().filter(|v| runs_on(&v.min_app_version, app)).max_by_key(|v| v.version)
    }

    /// The newest version of all.
    pub fn newest(&self) -> Option<&VersionInfo> {
        self.versions.iter().max_by_key(|v| v.version)
    }

    /// The preview to show for `version`: its own, or the newest earlier
    /// version's that has one (what the server sends for it).
    pub fn preview_for(&self, version: u32) -> Option<&VersionInfo> {
        self.versions
            .iter()
            .filter(|v| v.version <= version && matches!(v.preview, Preview::Still | Preview::Animated))
            .max_by_key(|v| v.version)
    }
}

/// `GET /api/v1/scripts/{id}`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScriptDetails {
    #[serde(flatten)]
    pub summary: ScriptSummary,
    pub versions: Vec<VersionInfo>,
    /// Whether the key asked about (`?viewer=`) has given it an encore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encored: Option<bool>,
}

/// Why something can be reported: (as the API names it, as shown).
pub const REPORT_REASONS: [(&str, &str); 6] = [
    ("malicious", "Malicious: meant to harm, freeze or trick"),
    ("derogatory", "Defamatory or derogatory content"),
    ("flashing", "Intense flashing without a warning"),
    ("copied", "Someone else's work, uncredited"),
    ("spam", "Spam"),
    ("other", "Other"),
];
pub const MAX_REPORT_DETAILS: usize = 2000;
/// Marked lines and sprites a report may point at.
pub const MAX_REPORT_MARKS: usize = 50;

/// How long ago a time (Unix seconds) was: "5 min ago".
pub fn ago(time: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let secs = (now - time).max(0);
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// Line ranges as written: "3-5, 9".
pub fn lines_text(lines: &[(u32, u32)]) -> String {
    lines
        .iter()
        .map(|&(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn report_reason_title(reason: &str) -> &str {
    REPORT_REASONS.iter().find(|(r, _)| *r == reason).map_or(reason, |(_, title)| title)
}

/// `POST /api/v1/scripts/{id}/report`, signed: what's wrong with a
/// version of it, and where (lines of its code, sprites in it by the line
/// they're registered on), for the server's admins.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ReportRequest {
    pub reason: String,
    pub details: String,
    pub version: u32,
    /// Line ranges (1-based, inclusive).
    #[serde(default)]
    pub lines: Vec<(u32, u32)>,
    /// Sprites, by the line their `sprite_register` is on.
    #[serde(default)]
    pub sprites: Vec<u32>,
}

/// A script's encores after giving or taking back one.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncoreState {
    pub encores: u64,
    pub encored: bool,
}

/// A report, as admins see it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub id: i64,
    pub script_id: String,
    /// Its name when it was reported (it may have gone since).
    pub script_name: String,
    #[serde(flatten)]
    pub request: ReportRequest,
    /// The reporter's ID.
    pub reporter_id: String,
    pub created: i64,
    pub resolved: Option<i64>,
    pub resolution: Option<String>,
    /// The script now: still there, hidden.
    pub exists: bool,
    pub hidden: bool,
}

/// What admins see of a script beyond its details.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AdminScriptInfo {
    #[serde(flatten)]
    pub summary: ScriptSummary,
    pub hidden: bool,
    pub hidden_reason: Option<String>,
    /// The uploader's key (hex), for a signed upload.
    pub author_key: Option<String>,
    /// Each version: its number, the address it came from (kept 30 days)
    /// and when.
    pub uploads: Vec<(u32, Option<String>, i64)>,
    pub reports: Vec<Report>,
}

/// A ban: a key (hex) or an address, until when (forever if not).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Ban {
    pub target: String,
    pub until: Option<i64>,
    pub reason: String,
    pub created: i64,
}

/// `POST /api/v1/admin`, signed by an admin's key (or the server's own,
/// which `synththing admin` uses): one moderation action.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum AdminAction {
    /// Whether the signer is an admin here (`{"admin": true}`).
    Whoami,
    /// Open reports (all, with `all`), newest first: `[Report]`.
    Reports { all: bool },
    /// `AdminScriptInfo`, hidden or not.
    Info { script: String },
    /// A version's source (the newest without one), hidden or not:
    /// `{"version": n, "source": "..."}`.
    Source { script: String, version: Option<u32> },
    Hide { script: String, reason: String },
    Unhide { script: String },
    /// Every version, for good (its reports stay).
    Delete { script: String },
    /// A key (hex, or an ID the server's seen) or an address, for `hours`
    /// (for good without), optionally hiding everything the key uploaded,
    /// and with `ban_addresses`, the addresses the key was seen on in the
    /// last 30 days, for 30 days, and any new one it shows up from while
    /// it's banned.
    Ban {
        target: String,
        hours: Option<u64>,
        reason: String,
        hide_scripts: bool,
        #[serde(default)]
        ban_addresses: bool,
    },
    Unban { target: String },
    /// `[Ban]`, those in force.
    Bans,
    AddAdmin { key: String },
    RemoveAdmin { key: String },
    /// `[[key, id]]`.
    Admins,
    /// Mark a report dealt with.
    Resolve { report: i64, note: String },
}

/// `POST /api/v1/scripts`: a new script.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Upload {
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub author_name: String,
    pub source: String,
    pub app_version: String,
    /// For a signed upload: its name among the uploader's scripts. One
    /// they've used before makes this a new version of that script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// The oldest app it runs on, as the publishing app worked it out: the
    /// server keeps the higher of this and its own (an app newer than the
    /// server knows functions the server doesn't).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_app_version: Option<String>,
}

/// What an accepted upload comes back with, signed by the server
/// (`receipt_message`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub id: String,
    pub version: u32,
    pub sha256: String,
    /// When it was accepted (Unix seconds).
    #[serde(default)]
    pub time: i64,
    #[serde(default)]
    pub signature: String,
}

/// `GET /api/v1/users/{id}`: someone who's posted signed uploads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UserInfo {
    pub id: String,
    /// The names they've posted under, most used first.
    pub names: Vec<String>,
    pub scripts: u64,
    pub encores: u64,
}

/// An error, as the API sends it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApiError {
    pub error: String,
    /// Seconds until trying again makes sense (rate limits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

/// Headers a script's source comes with: its version, its SHA-256, and the
/// server's signature over both (`source_message`).
pub const VERSION_HEADER: &str = "X-Synththing-Version";
pub const SHA256_HEADER: &str = "X-Synththing-Sha256";
pub const SIGNATURE_HEADER: &str = "X-Synththing-Signature";

/// Headers a signed request comes with: the signer's public key (hex),
/// the time (Unix seconds) and the signature over `request_message`.
pub const KEY_HEADER: &str = "X-Synththing-Key";
pub const TIME_HEADER: &str = "X-Synththing-Time";
/// A random value per signed request (hex), so no two signatures are the
/// same, and one seen again is a replay.
pub const NONCE_HEADER: &str = "X-Synththing-Nonce";

/// How far a signed request's time may be from the server's.
pub const REQUEST_WINDOW_SECONDS: i64 = 300;

/// What a signed request signs: which server, what's asked for, when,
/// its nonce and its body (by SHA-256), so it can't be replayed elsewhere
/// or later.
pub fn request_message(server_key: &str, method: &str, path: &str, time: i64, nonce: &str, body: &[u8]) -> Vec<u8> {
    format!("synththing/request/v1\n{server_key}\n{method}\n{path}\n{time}\n{nonce}\n{}", sha256_hex(body)).into_bytes()
}

/// A fresh nonce for a signed request.
pub fn new_nonce() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    hex(&bytes)
}

/// What a server signs for an accepted upload.
pub fn receipt_message(server_key: &str, receipt: &Receipt) -> Vec<u8> {
    format!(
        "synththing/receipt/v1\n{server_key}\n{}\n{}\n{}\n{}",
        receipt.id, receipt.version, receipt.sha256, receipt.time
    )
    .into_bytes()
}

/// The longest slug.
pub const MAX_SLUG_CHARS: usize = 40;

/// Whether `slug` is one: lowercase letters, digits and dashes, not
/// starting or ending with a dash.
pub fn is_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= MAX_SLUG_CHARS
        && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
}

/// A slug made from a name: "Neon Bars!" to "neon-bars".
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(MAX_SLUG_CHARS).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() { "script".into() } else { slug }
}

/// What a server signs for a script's source: which server, which script,
/// which version, and what's in it.
pub fn source_message(server_key: &str, id: &str, version: u32, sha256: &str) -> Vec<u8> {
    format!("synththing/source/v1\n{server_key}\n{id}\n{version}\n{sha256}").into_bytes()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// A new key pair's secret half.
pub fn new_signing_key() -> std::io::Result<SigningKey> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn sign_hex(key: &SigningKey, message: &[u8]) -> String {
    hex(&key.sign(message).to_bytes())
}

/// Whether `signature` (hex) is `key`'s (hex) over `message`.
pub fn verify_hex(key: &str, message: &[u8], signature: &str) -> bool {
    let (Some(key), Some(signature)) = (unhex(key), unhex(signature)) else { return false };
    let (Ok(key), Ok(signature)) = (<[u8; 32]>::try_from(key), <[u8; 64]>::try_from(signature)) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&key) else { return false };
    key.verify(message, &Signature::from_bytes(&signature)).is_ok()
}

/// A short, readable ID for a key (hex): base32 of the start of its
/// SHA-256 (`k3f9q2xa`), shown as `#k3f9q2xa`.
pub fn key_id(key: &str) -> String {
    let hash = Sha256::digest(key.as_bytes());
    base32(&hash[..5])
}

/// Lowercase base32 (RFC 4648 letters), no padding: 5 bytes make 8
/// characters.
pub fn base32(bytes: &[u8]) -> String {
    const LETTERS: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(LETTERS[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(LETTERS[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// What's wrong with an upload's text, if anything (the server checks
/// this; the app checks it first, to say so before sending).
pub fn check_upload(upload: &Upload) -> Result<(), String> {
    let name = upload.name.trim();
    if name.is_empty() {
        return Err("it needs a name".into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("the name is over {MAX_NAME_CHARS} characters"));
    }
    if upload.description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!("the description is over {MAX_DESCRIPTION_CHARS} characters"));
    }
    if !CATEGORIES.contains(&upload.category.as_str()) {
        return Err(format!("no category `{}`", upload.category));
    }
    if upload.tags.len() > MAX_TAGS {
        return Err(format!("more than {MAX_TAGS} tags"));
    }
    if let Some(tag) = upload.tags.iter().find(|t| t.trim().is_empty() || t.chars().count() > MAX_TAG_CHARS) {
        return Err(format!("the tag `{tag}` is empty or over {MAX_TAG_CHARS} characters"));
    }
    if upload.author_name.trim().is_empty() || upload.author_name.chars().count() > MAX_NAME_CHARS {
        return Err("it needs a name to post under (up to 60 characters)".into());
    }
    if upload.source.len() > MAX_SOURCE_BYTES {
        return Err(format!("the script is over {} KB", MAX_SOURCE_BYTES / 1024));
    }
    if let Some(slug) = &upload.slug
        && !is_slug(slug)
    {
        return Err(format!(
            "the slug `{slug}` can only have lowercase letters, digits and dashes (up to {MAX_SLUG_CHARS})"
        ));
    }
    if !upload.source.contains("function render") {
        return Err("that isn't a visualizer script (it has no render function)".into());
    }
    full_moon::parse(&upload.source).map_err(|_| "the script doesn't parse as Lua".to_string())?;
    Ok(())
}

/// Tags typed as one line ("chill, retro"): trimmed, lowercased, no
/// repeats.
pub fn parse_tags(text: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in text.split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// Where an installed (or published) script came from, kept beside it
/// (`<script>.lua.source.json`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Installed {
    /// The server's address.
    pub server: String,
    pub id: String,
    pub version: u32,
    /// The SHA-256 of the source as installed (or published), to tell
    /// whether it's been changed since.
    pub sha256: String,
    pub name: String,
    /// Its slug and author's ID, for a signed upload (yours, if you
    /// published it: a new upload with the slug is a new version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    /// The name its author posted under, and its category (for the script
    /// picker's sidebar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The server's signed receipt, for something you published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
}

pub fn installed_path(script: &std::path::Path) -> std::path::PathBuf {
    let mut name = script.as_os_str().to_os_string();
    name.push(".source.json");
    std::path::PathBuf::from(name)
}

impl Installed {
    pub fn read(script: &std::path::Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(installed_path(script)).ok()?).ok()
    }

    pub fn write(&self, script: &std::path::Path) -> std::io::Result<()> {
        std::fs::write(installed_path(script), serde_json::to_string_pretty(self).unwrap_or_default())
    }
}

/// Where the untouched copies of installed (and published) scripts are
/// kept, each named by its SHA-256: what an update is compared against,
/// and what Restore original puts back.
fn originals_dir() -> Option<std::path::PathBuf> {
    crate::config::config_dir().ok().map(|d| d.join("library").join("originals"))
}

/// Keep `source` as an original (once: it's named by its hash).
pub fn save_original(source: &str) {
    let Some(dir) = originals_dir() else { return };
    let path = dir.join(format!("{}.lua", sha256_hex(source.as_bytes())));
    if !path.exists() && std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(path, source);
    }
}

/// The original with this SHA-256, if it's kept (and intact).
pub fn read_original(sha256: &str) -> Option<String> {
    let text = std::fs::read_to_string(originals_dir()?.join(format!("{sha256}.lua"))).ok()?;
    (sha256_hex(text.as_bytes()) == sha256).then_some(text)
}

/// A script's own description: the comment lines it starts with, without
/// their `--`.
pub fn leading_comment(source: &str) -> String {
    let mut lines = Vec::new();
    for line in source.lines() {
        let Some(text) = line.trim_start().strip_prefix("--") else { break };
        if text.starts_with('[') {
            break; // (a block comment: not worth parsing for this)
        }
        lines.push(text.strip_prefix(' ').unwrap_or(text).trim_end());
    }
    lines.join("\n").trim().to_string()
}

/// A file name for a script called `name`: what Windows can't have in one
/// taken out.
pub fn file_stem_for(name: &str) -> String {
    let cleaned: String =
        name.chars().filter(|c| !c.is_control() && !r#"<>:"/\|?*"#.contains(*c)).collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().to_string();
    if cleaned.is_empty() { "script".to_string() } else { cleaned }
}

/// The synththing version each host function (and global) first shipped
/// in, for working out the oldest app a script runs on
/// (`min_app_version`). A function added since the last release gets the
/// version it will ship in, so scripts published from master in between
/// aren't offered to apps that don't have it yet. A test makes sure every
/// entry of `lua_completion::HOST_API` is here.
pub const API_SINCE: &[(&str, &str)] = &[
    ("clear", "0.1.1"),
    ("line", "0.1.1"),
    ("rect", "0.1.1"),
    ("text", "0.1.5"),
    ("text_size", "0.1.5"),
    ("FONT_HEIGHT", "0.1.5"),
    ("sprite_register", "0.1.8"),
    ("sprite", "0.1.8"),
    ("sprite_size", "0.1.8"),
    ("circle", "0.1.6"),
    ("triangle", "0.1.6"),
    ("polygon", "0.1.6"),
    ("pixel", "0.1.1"),
    ("fft_left", "0.1.1"),
    ("fft_right", "0.1.1"),
    ("beat", "0.1.6"),
    ("time_at_beat", "0.1.6"),
    ("bar", "0.1.6"),
    ("tempo", "0.1.6"),
    ("time_signature", "0.1.6"),
    ("level_left", "0.1.6"),
    ("level_right", "0.1.6"),
    ("onset", "0.1.6"),
    ("notes_between", "0.1.4"),
    ("active_notes", "0.1.1"),
    ("upcoming_notes", "0.1.1"),
    ("midi_channels", "0.1.1"),
    ("channel_enabled", "0.1.1"),
    ("set_channel_enabled", "0.1.1"),
    ("playback", "0.1.1"),
    ("set_paused", "0.1.4"),
    ("seek", "0.1.4"),
    ("script_options", "0.2.0"),
    ("recording", "0.4.0"),
    ("recording_ready", "0.4.0"),
    ("recording_done", "0.4.0"),
    ("set_speed", "0.2.0"),
    ("playlist", "0.2.0"),
    ("play_track", "0.2.0"),
    ("next_track", "0.2.0"),
    ("previous_track", "0.2.0"),
    ("set_loop", "0.2.0"),
    ("set_shuffle", "0.2.0"),
    ("store_get", "0.1.4"),
    ("store_set", "0.1.4"),
    ("log", "0.1.1"),
    ("log_app", "0.2.2"),
    ("debug_locals", "0.1.1"),
    ("setting_bool", "0.1.1"),
    ("setting_int", "0.1.1"),
    ("setting_float", "0.1.1"),
    ("setting_color", "0.1.1"),
    ("setting_string", "0.1.1"),
    ("setting_selection", "0.1.1"),
    ("mouse", "0.1.1"),
    ("mouse_delta", "0.1.1"),
    ("scroll", "0.1.1"),
    ("mouse_down", "0.1.1"),
    ("mouse_pressed", "0.1.1"),
    ("mouse_released", "0.1.1"),
    ("input_register", "0.1.8"),
    ("pad_axis", "0.2.1"),
    ("gamepads", "0.2.1"),
    ("input", "0.1.8"),
    ("input_down", "0.1.8"),
    ("input_value", "0.3.1"),
    ("text_typed", "0.1.8"),
    ("typing_begin", "0.1.8"),
    ("typing_state", "0.1.8"),
    ("typing_end", "0.1.8"),
    ("play_note", "0.1.8"),
    ("note_on", "0.1.8"),
    ("note_off", "0.1.8"),
    ("stop_notes", "0.1.8"),
    ("notes_playable", "0.1.8"),
    ("sequence_register", "0.1.8"),
    ("sequence_play", "0.1.8"),
    ("sequence_stop", "0.1.8"),
    ("has_focus", "0.1.1"),
    ("display_mode", "0.1.1"),
    ("set_cursor_visible", "0.1.1"),
    ("set_cursor_locked", "0.1.1"),
    ("SAMPLE_RATE", "0.1.1"),
    ("NOTE_LOOKAHEAD", "0.1.1"),
    ("DT", "0.1.1"),
    ("TIME", "0.1.4"),
    ("FRAME", "0.1.4"),
    ("coroutine", "0.4.0"),
];

/// The same for `script_options` keys (set inside a table, so looked for
/// by name).
pub const OPTION_SINCE: &[(&str, &str)] = &[
    ("start_paused", "0.2.0"),
    ("app_log", "0.2.2"),
    ("record_prepare", "0.4.0"),
    ("record_auto", "0.4.0"),
];

/// The oldest synththing version that has everything `source` uses: the
/// host functions and globals it calls (not its own names, and not a local
/// that happens to share a name), and the `script_options` keys it sets.
/// A call made indirectly (`_G["name"]`) isn't seen.
pub fn min_app_version(source: &str) -> String {
    let analysis = crate::lua_analysis::analyze(source, &Default::default());
    let mut min = semver::Version::new(0, 1, 1);
    let mut bump = |version: &str| {
        if let Ok(version) = semver::Version::parse(version)
            && version > min
        {
            min = version;
        }
    };
    for r in analysis.refs.iter().filter(|r| r.local.is_none() && !analysis.script_globals.contains(&r.name)) {
        if let Some((_, since)) = API_SINCE.iter().find(|(name, _)| *name == r.name) {
            bump(since);
        }
    }
    let words: std::collections::HashSet<&str> =
        source.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).collect();
    for (key, since) in OPTION_SINCE {
        if words.contains(key) {
            bump(since);
        }
    }
    min.to_string()
}

/// Whether an app at `app` (a version) can run something needing `needs`.
pub fn runs_on(needs: &str, app: &str) -> bool {
    match (semver::Version::parse(needs), semver::Version::parse(app)) {
        (Ok(needs), Ok(app)) => needs <= app,
        _ => true,
    }
}

/// One line of a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Same,
    /// In the old text only.
    Removed,
    /// In the new text only.
    Added,
}

/// The lines that differ between `old` and `new` (a longest-common-
/// subsequence diff), or `None` if they're too big to compare cheaply.
pub fn line_diff(old: &str, new: &str) -> Option<Vec<(Change, String)>> {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    // The same start and end needn't go through the table.
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a_mid, b_mid) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let (n, m) = (a_mid.len(), b_mid.len());
    if n.saturating_mul(m) > 4_000_000 {
        return None;
    }
    // lcs[i][j]: the longest common run of a_mid[i..] and b_mid[j..].
    let width = m + 1;
    let mut lcs = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if a_mid[i] == b_mid[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let mut out: Vec<(Change, String)> = a[..prefix].iter().map(|l| (Change::Same, l.to_string())).collect();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a_mid[i] == b_mid[j] {
            out.push((Change::Same, a_mid[i].to_string()));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1]) {
            // (Old lines before the new ones that replace them.)
            out.push((Change::Removed, a_mid[i].to_string()));
            i += 1;
        } else {
            out.push((Change::Added, b_mid[j].to_string()));
            j += 1;
        }
    }
    out.extend(a[a.len() - suffix..].iter().map(|l| (Change::Same, l.to_string())));
    Some(out)
}

/// One change from a base text: base lines `start..end` replaced by
/// `lines` (an insertion when the range is empty).
#[derive(Clone, Debug, PartialEq)]
struct Hunk {
    start: usize,
    end: usize,
    lines: Vec<String>,
}

/// The changes that turn `base` into `other`.
fn hunks(base: &str, other: &str) -> Option<Vec<Hunk>> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut at = 0;
    let mut open: Option<Hunk> = None;
    for (change, line) in line_diff(base, other)? {
        match change {
            Change::Same => {
                hunks.extend(open.take());
                at += 1;
            }
            Change::Removed => {
                open.get_or_insert(Hunk { start: at, end: at, lines: Vec::new() }).end += 1;
                at += 1;
            }
            Change::Added => open.get_or_insert(Hunk { start: at, end: at, lines: Vec::new() }).lines.push(line),
        }
    }
    hunks.extend(open);
    Some(hunks)
}

/// Both sets of changes to `base`, `ours` and `theirs`, in one text, when
/// they don't touch the same lines (a three-way merge, the way version
/// control does it); `None` when they do, or it's too big to work out.
pub fn merge(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let (ours_hunks, theirs_hunks) = (hunks(base, ours)?, hunks(base, theirs)?);
    let overlap = |a: &Hunk, b: &Hunk| {
        if a == b {
            return false; // (the same change on both sides)
        }
        match (a.start == a.end, b.start == b.end) {
            (true, true) => a.start == b.start,
            (true, false) => b.start < a.start && a.start < b.end,
            (false, true) => a.start < b.start && b.start < a.end,
            (false, false) => a.start < b.end && b.start < a.end,
        }
    };
    if ours_hunks.iter().any(|a| theirs_hunks.iter().any(|b| overlap(a, b))) {
        return None;
    }
    let mut all: Vec<Hunk> = ours_hunks;
    for hunk in theirs_hunks {
        if !all.contains(&hunk) {
            all.push(hunk);
        }
    }
    // Insertions before the lines they sit in front of.
    all.sort_by_key(|h| (h.start, h.end));
    let base_lines: Vec<&str> = base.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;
    for hunk in &all {
        out.extend(base_lines[at..hunk.start].iter().map(|l| l.to_string()));
        out.extend(hunk.lines.iter().cloned());
        at = hunk.end;
    }
    out.extend(base_lines[at..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    if theirs.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_and_ids() {
        let key = new_signing_key().unwrap();
        let public = hex(&key.verifying_key().to_bytes());
        let message = source_message(&public, "abc", 1, &sha256_hex(b"x"));
        let signature = sign_hex(&key, &message);
        assert!(verify_hex(&public, &message, &signature));
        assert!(!verify_hex(&public, &source_message(&public, "abc", 2, &sha256_hex(b"x")), &signature));
        assert!(!verify_hex("zz", &message, &signature));
        assert_eq!(key_id(&public).len(), 8);
        assert_eq!(base32(b"f"), "my");
        assert_eq!(base32(b"fooba"), "mzxw6ytb");
        assert_eq!(unhex(&hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
    }

    #[test]
    fn uploads_are_checked() {
        let good = Upload {
            name: "Bars".into(),
            description: "bars".into(),
            category: "visualizer".into(),
            tags: parse_tags("Chill, retro, chill"),
            author_name: "me".into(),
            source: "function render() end".into(),
            app_version: "0.4.0".into(),
            slug: Some("bars".into()),
            min_app_version: None,
        };
        assert_eq!(good.tags, ["chill", "retro"]);
        assert_eq!(check_upload(&good), Ok(()));
        let bad = |change: fn(&mut Upload)| {
            let mut upload = good.clone();
            change(&mut upload);
            check_upload(&upload).unwrap_err()
        };
        assert!(bad(|u| u.name = " ".into()).contains("name"));
        assert!(bad(|u| u.category = "music".into()).contains("category"));
        assert!(bad(|u| u.source = "local x = 1".into()).contains("render"));
        assert!(bad(|u| u.source = "function render() end end".into()).contains("parse"));
        assert!(bad(|u| u.slug = Some("Bad Slug".into())).contains("slug"));
        assert_eq!(slugify(" Neon  Bars! v2 "), "neon-bars-v2");
        assert_eq!(slugify("!!!"), "script");
        assert!(is_slug("neon-bars") && !is_slug("-x") && !is_slug("a_b"));
    }

    #[test]
    fn diffs() {
        let diff = line_diff("a\nb\nc\nd", "a\nB\nc\nd\ne").unwrap();
        let changed: Vec<(Change, &str)> =
            diff.iter().filter(|(c, _)| *c != Change::Same).map(|(c, l)| (*c, l.as_str())).collect();
        assert_eq!(changed, [(Change::Removed, "b"), (Change::Added, "B"), (Change::Added, "e")]);
        assert_eq!(diff.len(), 6);
        assert!(line_diff("same", "same").unwrap().iter().all(|(c, _)| *c == Change::Same));
    }

    #[test]
    fn bundled_scripts_make_valid_uploads() {
        for &(category, file_name, source) in crate::lua_visualizer::BUNDLED_SCRIPTS {
            match bundled_upload(category, file_name, source) {
                Some(upload) => assert_eq!(check_upload(&upload), Ok(()), "{file_name}"),
                None => assert!(category == "templates" || file_name == "editor_test.lua", "{file_name}"),
            }
        }
        assert_eq!(official_id().len(), 8);
    }

    #[test]
    fn bundled_scripts_read_from_the_repository() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/visualizers");
        let from_disk = bundled_scripts(Some(&dir)).unwrap();
        let built_in = bundled_scripts(None).unwrap();
        assert_eq!(from_disk.len(), built_in.len());
        for (category, name, _) in &built_in {
            assert!(from_disk.iter().any(|(c, n, _)| c == category && n == name), "{category}/{name}");
        }
    }

    #[test]
    fn minimum_app_versions() {
        // Every host function has a version, and a sane one.
        for (name, _) in crate::lua_completion::HOST_API.iter().filter(|(n, _)| *n != "render") {
            assert!(API_SINCE.iter().any(|(n, _)| n == name), "{name} isn't in API_SINCE");
        }
        for (name, version) in API_SINCE.iter().chain(OPTION_SINCE) {
            assert!(semver::Version::parse(version).is_ok(), "{name}: {version}");
        }
        assert_eq!(min_app_version("function render() clear({ r = 0, g = 0, b = 0 }) end"), "0.1.1");
        assert_eq!(min_app_version("function render() local r = recording() end"), "0.4.0");
        assert_eq!(min_app_version("script_options({ app_log = true }) function render() end"), "0.2.2");
        // A local (or a field) that shares a name doesn't count.
        let shadowed = "local recording = false function render() local t = { input = 1 } return recording end";
        assert_eq!(min_app_version(shadowed), "0.1.1");
        // Nor does the script's own global function.
        assert_eq!(min_app_version("function seek() end function render() seek() end"), "0.1.1");
        assert!(runs_on("0.4.0", "0.4.1") && runs_on("0.4.0", "0.4.0") && !runs_on("0.4.1", "0.4.0"));
        // Names this build doesn't know (a newer function), but not the
        // script's own, Lua's, or locals.
        let newer = "local x = 1 function helper() end function render() helper() brand_new_thing() math.floor(x) end";
        assert_eq!(unknown_globals(newer), ["brand_new_thing"]);
        // (editor_test.lua misspells one on purpose, and isn't published.)
        for &(_, name, source) in crate::lua_visualizer::BUNDLED_SCRIPTS.iter().filter(|(_, n, _)| *n != "editor_test.lua") {
            assert!(unknown_globals(source).is_empty(), "{name}: {:?}", unknown_globals(source));
        }
    }

    #[test]
    fn merges() {
        let base = "speed 0.4\nlocal x\npixel white\nend\n";
        // Theirs: a new speed, and a line after pixel; ours: green.
        let theirs = "speed 1.0\nlocal x\npixel white\ntwinkle\nend\n";
        let ours = "speed 0.4\nlocal x\npixel green\nend\n";
        assert_eq!(merge(base, ours, theirs).as_deref(), Some("speed 1.0\nlocal x\npixel green\ntwinkle\nend\n"));
        // Both changing the same line: no.
        assert_eq!(merge(base, "speed 9\nlocal x\npixel white\nend\n", theirs), None);
        // The same change on both sides is fine.
        assert_eq!(merge(base, theirs, theirs).as_deref(), Some(theirs));
        // Both adding at the same spot: no.
        assert_eq!(merge("a\nb", "a\nx\nb", "a\ny\nb"), None);
    }

    #[test]
    fn descriptions_and_file_names() {
        assert_eq!(leading_comment("-- Bars: a visualizer.\n--   indented\n--\n-- more\nlocal x = 1\n-- not this"), "Bars: a visualizer.\n  indented\n\nmore");
        assert_eq!(leading_comment("local x = 1"), "");
        assert_eq!(file_stem_for(" Neon: Bars?. "), "Neon Bars");
        assert_eq!(file_stem_for("///"), "script");
    }
}
