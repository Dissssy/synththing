//! The script library (see docs/SERVER.md): servers holding shared
//! visualizer scripts, and the app's side of talking to them.
//!
//! This module has what both sides agree on: the JSON the API sends and
//! takes, the categories, and how a server signs what it serves. `server`
//! is `synththing serve`; `client` is the app's requests.

pub mod client;
pub mod server;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The official server.
pub const OFFICIAL_URL: &str = "https://synththing.p51.nl";

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
pub const MAX_DESCRIPTION_CHARS: usize = 2000;
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
    pub created: i64,
}

/// `GET /api/v1/scripts/{id}`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScriptDetails {
    #[serde(flatten)]
    pub summary: ScriptSummary,
    pub versions: Vec<VersionInfo>,
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
}

/// What an accepted upload comes back with.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub id: String,
    pub version: u32,
    pub sha256: String,
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
    lines.join("
").trim().to_string()
}

/// A file name for a script called `name`: what Windows can't have in one
/// taken out.
pub fn file_stem_for(name: &str) -> String {
    let cleaned: String =
        name.chars().filter(|c| !c.is_control() && !r#"<>:"/\|?*"#.contains(*c)).collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().to_string();
    if cleaned.is_empty() { "script".to_string() } else { cleaned }
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
    }

    #[test]
    fn descriptions_and_file_names() {
        assert_eq!(leading_comment("-- Bars: a visualizer.
--   indented
--
-- more
local x = 1
-- not this"), "Bars: a visualizer.
  indented

more");
        assert_eq!(leading_comment("local x = 1"), "");
        assert_eq!(file_stem_for(" Neon: Bars?. "), "Neon Bars");
        assert_eq!(file_stem_for("///"), "script");
    }
}
