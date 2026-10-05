//! The script library (see docs/SERVER.md): servers holding shared
//! visualizer scripts, and the app's side of talking to them.
//!
//! This module has what both sides agree on: the JSON the API sends and
//! takes, the categories, and how a server signs what it serves. `server`
//! is `synththing serve`; `client` is the app's requests.

pub mod client;
pub mod identity;
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
    /// For a signed upload: its name among the uploader's scripts. One
    /// they've used before makes this a new version of that script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
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
