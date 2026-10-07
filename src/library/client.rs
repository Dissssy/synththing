//! The app's side of a library server: one function per request, blocking
//! (the app runs them on threads of its own). Errors come back as a line
//! to show: the server's own message when it sent one.
//!
//! A server's key is pinned by the app the first time it's seen (see
//! `Info::key`); sources are only accepted signed by that key.

use std::sync::OnceLock;
use std::time::Duration;

use ureq::Agent;

use super::identity::Identity;
use super::{
    receipt_message, request_message, source_message, verify_hex, AdminAction, ApiError, EncoreState, Info, Listing,
    Receipt, ReportRequest, ScriptDetails, Upload, UserInfo, KEY_HEADER, NONCE_HEADER, SHA256_HEADER,
    SIGNATURE_HEADER, TIME_HEADER, VERSION_HEADER,
};

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent(format!("synththing/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

/// `https://example.org/` and `example.org` both to `https://example.org`
/// (plain `http://` only for addresses written with it).
pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

type Reply = ureq::http::Response<ureq::Body>;

/// The response, or the error it is (the server's message if it sent one).
fn check(result: Result<Reply, ureq::Error>) -> Result<Reply, String> {
    let mut response = result.map_err(|e| format!("couldn't reach the server ({e})"))?;
    let status = response.status().as_u16();
    if status < 400 {
        return Ok(response);
    }
    match response.body_mut().read_json::<ApiError>() {
        Ok(ApiError { error, retry_after: Some(wait) }) => {
            Err(format!("{error} (try again in {})", crate::song_info::format_length(wait as f64)))
        }
        Ok(ApiError { error, .. }) => Err(error),
        Err(_) => Err(format!("the server said {status}")),
    }
}

fn read<T: serde::de::DeserializeOwned>(mut response: Reply) -> Result<T, String> {
    response.body_mut().read_json().map_err(|e| format!("the server sent something unexpected ({e})"))
}

pub fn info(base: &str) -> Result<Info, String> {
    read(check(agent().get(format!("{base}/api/v1/info")).call())?)
}

/// What to list.
#[derive(Clone, Debug, Default)]
pub struct Search {
    pub query: String,
    pub category: Option<&'static str>,
    /// `"new"` or `"top"`.
    pub sort: &'static str,
    /// Only this user's uploads (their ID).
    pub author: Option<String>,
    pub page: u32,
}

/// A page of scripts.
pub fn list(base: &str, search: &Search) -> Result<Listing, String> {
    let sort = if search.sort.is_empty() { "new" } else { search.sort };
    let mut request =
        agent().get(format!("{base}/api/v1/scripts")).query("sort", sort).query("page", search.page.to_string());
    if !search.query.trim().is_empty() {
        request = request.query("q", search.query.trim());
    }
    if let Some(category) = search.category {
        request = request.query("category", category);
    }
    if let Some(author) = &search.author {
        request = request.query("author", author);
    }
    read(check(request.call())?)
}

/// One of a server's documents (`Info::documents`), as Markdown.
pub fn document(base: &str, slug: &str) -> Result<String, String> {
    let mut response = check(agent().get(format!("{base}/api/v1/documents/{slug}")).call())?;
    response
        .body_mut()
        .with_config()
        .limit(512 * 1024)
        .read_to_string()
        .map_err(|e| format!("couldn't read it ({e})"))
}

pub fn user(base: &str, id: &str) -> Result<UserInfo, String> {
    read(check(agent().get(format!("{base}/api/v1/users/{id}")).call())?)
}

/// `author_id`'s script with `slug`, if they have one.
pub fn by_slug(base: &str, author_id: &str, slug: &str) -> Result<Option<super::ScriptSummary>, String> {
    let response = agent().get(format!("{base}/api/v1/users/{author_id}/scripts/{slug}")).call();
    match response {
        Ok(r) if r.status().as_u16() == 404 => Ok(None),
        other => read(check(other)?).map(Some),
    }
}

pub fn details(base: &str, id: &str) -> Result<ScriptDetails, String> {
    details_as(base, id, None)
}

/// The same, saying whether `viewer` (a public key, hex) gave it an encore.
pub fn details_as(base: &str, id: &str, viewer: Option<&str>) -> Result<ScriptDetails, String> {
    let mut request = agent().get(format!("{base}/api/v1/scripts/{id}"));
    if let Some(viewer) = viewer {
        request = request.query("viewer", viewer);
    }
    read(check(request.call())?)
}

/// `request`, signed by `identity` for the server whose key is `server_key`.
fn signed<B>(
    request: ureq::RequestBuilder<B>,
    server_key: &str,
    identity: &Identity,
    method: &str,
    path: &str,
    body: &[u8],
) -> ureq::RequestBuilder<B> {
    let (time, nonce) = (now(), super::new_nonce());
    let signature = identity.sign(&request_message(server_key, method, path, time, &nonce, body));
    request
        .header(KEY_HEADER, identity.public())
        .header(TIME_HEADER, time.to_string())
        .header(NONCE_HEADER, nonce)
        .header(SIGNATURE_HEADER, signature)
}

/// Give a script an encore (`give`), or take it back.
pub fn encore(base: &str, server_key: &str, id: &str, identity: &Identity, give: bool) -> Result<EncoreState, String> {
    let path = format!("/api/v1/scripts/{id}/encore");
    let url = format!("{base}{path}");
    let response = if give {
        signed(agent().post(url), server_key, identity, "POST", &path, &[]).send_empty()
    } else {
        signed(agent().delete(url), server_key, identity, "DELETE", &path, &[]).call()
    };
    read(check(response)?)
}

/// A rotation record the authority (`server_key`) sent back, checked.
fn checked_rotation(server_key: &str, record: super::rotation::Rotation) -> Result<super::rotation::Rotation, String> {
    if record.verify(&[server_key.to_string()]) {
        Ok(record)
    } else {
        Err("the authority's rotation record isn't signed by its key".into())
    }
}

/// Replace `old` with `new` at the authority whose key is `server_key`:
/// the record of it, to keep (and show other servers).
pub fn rotate(base: &str, server_key: &str, old: &Identity, new: &Identity) -> Result<super::rotation::Rotation, String> {
    let time = now();
    let ask = super::rotation::RotateRequest {
        new: new.public(),
        time,
        new_signature: new.sign(&super::rotation::new_key_message(server_key, &old.public(), &new.public(), time)),
    };
    let path = "/api/v1/identity/rotate";
    let body = serde_json::to_vec(&ask).map_err(|e| e.to_string())?;
    let request = agent().post(format!("{base}{path}")).header("Content-Type", "application/json");
    let record = read(check(signed(request, server_key, old, "POST", path, &body).send(&body[..]))?)?;
    checked_rotation(server_key, record)
}

/// The rotations that led to `key` (oldest first), from the authority,
/// each checked against its key.
pub fn lineage(base: &str, server_key: &str, key: &str) -> Result<Vec<super::rotation::Rotation>, String> {
    let chain: Vec<super::rotation::Rotation> =
        read(check(agent().get(format!("{base}/api/v1/identity/rotations")).query("key", key).call())?)?;
    super::rotation::verify_chain(&chain, &[server_key.to_string()])?;
    if chain.last().is_some_and(|last| last.new != key) {
        return Err("the authority sent rotations for another key".into());
    }
    Ok(chain)
}

/// Show a server the rotations an identity went through (oldest first):
/// how many were new to it.
pub fn apply(base: &str, chain: &[super::rotation::Rotation]) -> Result<u64, String> {
    let reply: serde_json::Value = read(check(agent().post(format!("{base}/api/v1/identity/apply")).send_json(chain))?)?;
    Ok(reply["applied"].as_u64().unwrap_or(0))
}

/// What the authority knows of `identity`.
pub fn identity_status(base: &str, server_key: &str, identity: &Identity) -> Result<super::rotation::IdentityStatus, String> {
    let path = "/api/v1/identity/status";
    let request = agent().post(format!("{base}{path}")).header("Content-Type", "application/json");
    read(check(signed(request, server_key, identity, "POST", path, b"{}").send(&b"{}"[..]))?)
}

/// Ask for codes to attach, change or take off `identity`'s email: which
/// addresses got one ("new", "current").
pub fn email(
    base: &str,
    server_key: &str,
    identity: &Identity,
    ask: &super::rotation::EmailRequest,
) -> Result<Vec<String>, String> {
    let path = "/api/v1/identity/email";
    let body = serde_json::to_vec(ask).map_err(|e| e.to_string())?;
    let request = agent().post(format!("{base}{path}")).header("Content-Type", "application/json");
    let reply: serde_json::Value = read(check(signed(request, server_key, identity, "POST", path, &body).send(&body[..]))?)?;
    Ok(reply["sent"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default())
}

/// The codes, to finish an email change: whether one's attached now.
pub fn email_confirm(
    base: &str,
    server_key: &str,
    identity: &Identity,
    confirm: &super::rotation::EmailConfirm,
) -> Result<bool, String> {
    let path = "/api/v1/identity/email/confirm";
    let body = serde_json::to_vec(confirm).map_err(|e| e.to_string())?;
    let request = agent().post(format!("{base}{path}")).header("Content-Type", "application/json");
    let status: super::rotation::IdentityStatus =
        read(check(signed(request, server_key, identity, "POST", path, &body).send(&body[..]))?)?;
    Ok(status.email)
}

/// Ask to recover the identity `address` is attached to, to `new`: a code
/// goes to the address (if it's attached to one).
pub fn recover(base: &str, server_key: &str, address: &str, new: &Identity) -> Result<(), String> {
    let time = now();
    let ask = super::rotation::RecoverRequest {
        address: address.trim().to_string(),
        new: new.public(),
        time,
        new_signature: new.sign(&super::rotation::new_key_message(server_key, "", &new.public(), time)),
    };
    check(agent().post(format!("{base}/api/v1/identity/recover")).send_json(&ask))?;
    Ok(())
}

/// The emailed code, to finish a recovery to `new`: the rotation record.
pub fn recover_confirm(base: &str, server_key: &str, new: &Identity, code: &str) -> Result<super::rotation::Rotation, String> {
    let confirm = super::rotation::RecoverConfirm { new: new.public(), code: code.trim().to_string() };
    let record = read(check(agent().post(format!("{base}/api/v1/identity/recover/confirm")).send_json(&confirm))?)?;
    checked_rotation(server_key, record)
}

/// How a request to delete an identity's data went.
#[derive(Clone, Debug, Default, serde::Deserialize, PartialEq)]
pub struct Deletion {
    /// Deleted: for good after this time (Unix seconds), unless restored.
    #[serde(default)]
    pub deleted: Option<i64>,
    /// A code went to the attached email: confirm with it.
    #[serde(default)]
    pub sent: bool,
}

fn signed_post<T: serde::de::DeserializeOwned>(
    base: &str,
    server_key: &str,
    identity: &Identity,
    path: &str,
    body: &serde_json::Value,
) -> Result<T, String> {
    let body = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let request = agent().post(format!("{base}{path}")).header("Content-Type", "application/json");
    read(check(signed(request, server_key, identity, "POST", path, &body).send(&body[..]))?)
}

/// Delete what the server has of `identity`'s (undoable for 30 days):
/// where it has an email attached, `address` has to be it, and a code
/// goes to it first (`delete_confirm`).
pub fn delete_identity(base: &str, server_key: &str, identity: &Identity, address: Option<&str>) -> Result<Deletion, String> {
    signed_post(base, server_key, identity, "/api/v1/identity/delete", &serde_json::json!({ "address": address }))
}

/// The emailed code, to finish a deletion.
pub fn delete_confirm(base: &str, server_key: &str, identity: &Identity, code: &str) -> Result<Deletion, String> {
    signed_post(base, server_key, identity, "/api/v1/identity/delete/confirm", &serde_json::json!({ "code": code.trim() }))
}

/// Undo a deletion: how many scripts came back.
pub fn restore_identity(base: &str, server_key: &str, identity: &Identity) -> Result<u64, String> {
    let reply: serde_json::Value = signed_post(base, server_key, identity, "/api/v1/identity/restore", &serde_json::json!({}))?;
    Ok(reply["restored"].as_u64().unwrap_or(0))
}

/// `identity`'s notifications on a server, newest first (after `after`).
pub fn notifications(
    base: &str,
    server_key: &str,
    identity: &Identity,
    after: Option<i64>,
) -> Result<Vec<super::Notification>, String> {
    signed_post(base, server_key, identity, "/api/v1/notifications", &serde_json::json!({ "after": after }))
}

/// Mark notifications read (or `handled`): these, or `all` of them.
pub fn mark_notifications(
    base: &str,
    server_key: &str,
    identity: &Identity,
    ids: &[i64],
    all: bool,
    handled: bool,
) -> Result<(), String> {
    let body = serde_json::json!({ "ids": ids, "all": all, "handled": handled });
    signed_post::<serde_json::Value>(base, server_key, identity, "/api/v1/notifications/mark", &body).map(|_| ())
}

/// Listen for new notifications, calling `each` with every one as it
/// comes, until the server or the connection ends it, `STREAM_FOR` is up
/// (ureq's body timeout is for the whole body: the caller reconnects), or
/// `each` says to stop, with false.
pub fn notification_stream(
    base: &str,
    server_key: &str,
    identity: &Identity,
    mut each: impl FnMut(super::Notification) -> bool,
) -> Result<(), String> {
    use std::io::BufRead;
    static STREAMING: OnceLock<Agent> = OnceLock::new();
    let agent = STREAMING.get_or_init(|| {
        Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_body(Some(STREAM_FOR))
            .user_agent(format!("synththing/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    });
    let path = "/api/v1/notifications/stream";
    let response = check(signed(agent.get(format!("{base}{path}")), server_key, identity, "GET", path, &[]).call())?;
    let reader = std::io::BufReader::new(response.into_body().into_reader());
    for line in reader.lines() {
        let line = line.map_err(|e| format!("the stream ended ({e})"))?;
        if let Some(data) = line.strip_prefix("data: ")
            && let Ok(notification) = serde_json::from_str(data)
            && !each(notification)
        {
            return Ok(());
        }
    }
    Ok(())
}

/// How long one notification stream is kept before it's opened again (so a
/// connection that's quietly died is noticed).
pub const STREAM_FOR: Duration = Duration::from_secs(10 * 60);

/// Report a script to the server's admins; the report's number.
pub fn report(base: &str, server_key: &str, id: &str, identity: &Identity, report: &ReportRequest) -> Result<i64, String> {
    let path = format!("/api/v1/scripts/{id}/report");
    let body = serde_json::to_vec(report).map_err(|e| e.to_string())?;
    let request = agent().post(format!("{base}{path}")).header("Content-Type", "application/json");
    let reply: serde_json::Value = read(check(signed(request, server_key, identity, "POST", &path, &body).send(&body[..]))?)?;
    reply["report"].as_i64().ok_or_else(|| "the server sent something unexpected".to_string())
}

/// An admin action, signed by `identity` (an admin's, or the server's own).
pub fn admin<T: serde::de::DeserializeOwned>(
    base: &str,
    server_key: &str,
    identity: &Identity,
    action: &AdminAction,
) -> Result<T, String> {
    let path = "/api/v1/admin";
    let body = serde_json::to_vec(action).map_err(|e| e.to_string())?;
    let request = agent().post(format!("{base}{path}")).header("Content-Type", "application/json");
    read(check(signed(request, server_key, identity, "POST", path, &body).send(&body[..]))?)
}

/// A script's preview for `version` (the newest version's up to it that
/// has one): the animation (`sheet`), or the still. PNG.
pub fn preview(base: &str, id: &str, version: u32, sheet: bool) -> Result<Vec<u8>, String> {
    let file = if sheet { "preview-sheet.png" } else { "preview.png" };
    let request = agent().get(format!("{base}/api/v1/scripts/{id}/{file}")).query("version", version.to_string());
    let mut response = check(request.call())?;
    response
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("couldn't read the preview ({e})"))
}

/// A downloaded script, checked against the server's signature.
#[derive(Clone, Debug)]
pub struct Download {
    pub source: String,
    pub version: u32,
    pub sha256: String,
}

/// A script's source (`version`, or its newest), accepted only signed by
/// `server_key` (hex) and matching the hash signed.
pub fn download(base: &str, id: &str, version: Option<u32>, server_key: &str) -> Result<Download, String> {
    let mut request = agent().get(format!("{base}/api/v1/scripts/{id}/source"));
    if let Some(version) = version {
        request = request.query("version", version.to_string());
    }
    let mut response = check(request.call())?;
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let (Some(version), Some(sha256), Some(signature)) = (header(VERSION_HEADER), header(SHA256_HEADER), header(SIGNATURE_HEADER))
    else {
        return Err("the server didn't sign the script".into());
    };
    let version: u32 = version.parse().map_err(|_| "the server sent a bad version".to_string())?;
    let source = response.body_mut().read_to_string().map_err(|e| format!("couldn't read the script ({e})"))?;
    if super::sha256_hex(source.as_bytes()) != sha256 {
        return Err("the script didn't arrive whole (its hash doesn't match)".into());
    }
    if !verify_hex(server_key, &source_message(server_key, id, version, &sha256), &signature) {
        return Err("the script's signature doesn't match the server's key".into());
    }
    Ok(Download { source, version, sha256 })
}

/// Upload a script to the server whose key is `server_key`: signed by
/// `identity`, or anonymously without one. The receipt is checked against
/// the server's key.
pub fn upload(base: &str, server_key: &str, upload: &Upload, identity: Option<&Identity>) -> Result<Receipt, String> {
    let body = serde_json::to_vec(upload).map_err(|e| e.to_string())?;
    let mut request = agent().post(format!("{base}/api/v1/scripts")).header("Content-Type", "application/json");
    if let Some(identity) = identity {
        let (time, nonce) = (now(), super::new_nonce());
        let signature = identity.sign(&request_message(server_key, "POST", "/api/v1/scripts", time, &nonce, &body));
        request = request
            .header(KEY_HEADER, identity.public())
            .header(TIME_HEADER, time.to_string())
            .header(NONCE_HEADER, nonce)
            .header(SIGNATURE_HEADER, signature);
    }
    let receipt: Receipt = read(check(request.send(&body[..]))?)?;
    if !verify_hex(server_key, &receipt_message(server_key, &receipt), &receipt.signature) {
        return Err("the server's receipt isn't signed by its key".into());
    }
    Ok(receipt)
}

/// Delete one of `identity`'s scripts.
pub fn delete(base: &str, server_key: &str, id: &str, identity: &Identity) -> Result<(), String> {
    let path = format!("/api/v1/scripts/{id}");
    let (time, nonce) = (now(), super::new_nonce());
    let signature = identity.sign(&request_message(server_key, "DELETE", &path, time, &nonce, &[]));
    check(
        agent()
            .delete(format!("{base}{path}"))
            .header(KEY_HEADER, identity.public())
            .header(TIME_HEADER, time.to_string())
            .header(NONCE_HEADER, nonce)
            .header(SIGNATURE_HEADER, signature)
            .call(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{server, Upload};

    fn upload_of(name: &str, category: &str, source: &str) -> Upload {
        Upload {
            name: name.into(),
            description: format!("{name} does things"),
            category: category.into(),
            tags: vec!["chill".into()],
            author_name: "tester".into(),
            source: source.into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            slug: None,
            min_app_version: None,
            remix_of: None,
        }
    }

    fn search(query: &str, category: Option<&'static str>, sort: &'static str) -> Search {
        Search { query: query.into(), category, sort, ..Default::default() }
    }

    /// A real server on a free port, and the app's requests against it.
    #[test]
    fn uploads_lists_searches_and_downloads() {
        let dir = std::env::temp_dir().join(format!("synththing-server-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);

        let info = info(&base).unwrap();
        assert_eq!((info.mode.as_str(), info.license.as_str()), ("open", "CC-BY-4.0"));
        assert!(info.documents.is_empty());
        assert_eq!(info.key.len(), 64);

        let bars = upload(&base, &info.key, &upload_of("Neon Bars", "visualizer", "function render() clear({r=1,g=2,b=3}) end"), None).unwrap();
        let game = upload(&base, &info.key, &upload_of("Space Game", "game", "function render() end -- pew"), None).unwrap();
        assert_eq!(bars.version, 1);
        assert_ne!(bars.id, game.id);
        // Refused: not a script.
        let refused = upload(&base, &info.key, &upload_of("Nope", "visualizer", "print('hi')"), None).unwrap_err();
        assert!(refused.contains("render"), "{refused}");

        let all = list(&base, &search("", None, "new")).unwrap();
        assert_eq!(all.total, 2);
        assert_eq!(all.scripts[0].name, "Space Game", "newest first");
        assert_eq!(list(&base, &search("neo", None, "new")).unwrap().scripts[0].id, bars.id, "prefix search");
        assert_eq!(list(&base, &search("\"pew", None, "new")).unwrap().total, 0, "quotes are just dropped");
        assert_eq!(list(&base, &search("", Some("game"), "top")).unwrap().scripts[0].id, game.id);
        assert_eq!(list(&base, &search("things", Some("toy"), "new")).unwrap().total, 0);

        let details = details(&base, &bars.id).unwrap();
        assert_eq!(details.summary.author_id, None, "anonymous");
        assert_eq!(details.versions.len(), 1);
        let got = download(&base, &bars.id, None, &info.key).unwrap();
        assert_eq!(got.source, "function render() clear({r=1,g=2,b=3}) end");
        assert_eq!(got.sha256, bars.sha256);
        // Another key: refused.
        let other = crate::library::hex(&crate::library::new_signing_key().unwrap().verifying_key().to_bytes());
        assert!(download(&base, &bars.id, None, &other).unwrap_err().contains("signature"));
        assert!(download(&base, "nothere", None, &info.key).unwrap_err().contains("no such script"));

        running.stop();
        // Documents, offered from the settings.
        std::fs::write(dir.join("eula.md"), "# EULA\n\nBe *nice*.\n").unwrap();
        let mut config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("server.json")).unwrap()).unwrap();
        config["documents"] = serde_json::json!([
            { "title": "EULA", "icon": "scroll", "file": "eula.md" },
            { "title": "Privacy Policy", "icon": "shield-check", "file": "missing.md" }
        ]);
        std::fs::write(dir.join("server.json"), config.to_string()).unwrap();
        // The data stays: a restart serves the same scripts with the same key.
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        assert_eq!(super::info(&base).unwrap().key, info.key);
        let documents = super::info(&base).unwrap().documents;
        assert_eq!(documents.iter().map(|d| d.slug.as_str()).collect::<Vec<_>>(), ["eula", "privacy-policy"]);
        assert_eq!(document(&base, "eula").unwrap(), "# EULA\n\nBe *nice*.\n");
        assert!(document(&base, "privacy-policy").unwrap_err().contains("missing"));
        assert!(document(&base, "nope").unwrap_err().contains("no such"));
        assert_eq!(list(&base, &search("", None, "new")).unwrap().total, 2);
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each upload's preview is made after it: pending, then animated (or
    /// only a still); a later version without one yet shows the earlier's.
    #[test]
    fn previews_are_made_for_uploads() {
        let dir = std::env::temp_dir().join(format!("synththing-preview-server-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = info(&base).unwrap().key;
        let wait_for = |id: &str, version: u32| {
            for _ in 0..600 {
                let details = details(&base, id).unwrap();
                let v = details.versions.iter().find(|v| v.version == version).unwrap();
                if v.preview != crate::library::Preview::Pending {
                    return v.preview;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("no preview of {id} v{version}");
        };
        let moving = "function render(w, h) clear({ r = 0, g = 0, b = 0 }) \
                      rect((FRAME * 5) % w, 10, (FRAME * 5) % w + 30, 40, { r = 255, g = 200, b = 0 }) end";
        let bars = upload(&base, &key, &upload_of("Moving", "visualizer", moving), None).unwrap();
        let quitter = "function render() rect(0, 0, 9, 9, { r = 255, g = 0, b = 0 }) recording_done() end";
        let blank = upload(&base, &key, &upload_of("Quitter", "visualizer", quitter), None).unwrap();
        let nothing = upload(&base, &key, &upload_of("Nothing", "visualizer", "function render() end"), None).unwrap();
        assert_eq!(wait_for(&bars.id, 1), crate::library::Preview::Animated);
        assert_eq!(wait_for(&blank.id, 1), crate::library::Preview::Still);
        assert_eq!(wait_for(&nothing.id, 1), crate::library::Preview::None);
        assert!(preview(&base, &nothing.id, 1, false).unwrap_err().contains("no preview"));
        // One that doesn't run isn't taken.
        let broken = upload_of("Broken", "visualizer", "function render() if FRAME > 2 then nope() end end");
        let refused = upload(&base, &key, &broken, None).unwrap_err();
        assert!(refused.contains("doesn't run") && refused.contains("nope"), "{refused}");
        let sheet = preview(&base, &bars.id, 1, true).unwrap();
        let (w, h, _) = crate::preview::decode_png(&sheet).unwrap();
        assert_eq!((w, h), (crate::preview::WIDTH * crate::preview::COLUMNS, crate::preview::HEIGHT * 6));
        assert!(crate::preview::decode_png(&preview(&base, &blank.id, 1, false).unwrap()).is_ok());
        assert!(preview(&base, &blank.id, 1, true).unwrap_err().contains("no preview"));
        // A later version asked for: the newest one up to it with a preview.
        assert!(preview(&base, &bars.id, 7, false).is_ok());
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Signed uploads: a slug used again is a new version, owned by its
    /// key; deletes need it; signed-only servers refuse anonymous uploads.
    /// Remixes link both ways; near-copies of someone else's script (or a
    /// bundled one) are refused unless they're remixes of it, and remixes
    /// with no changes are refused; your own scripts aren't copies.
    #[test]
    fn remixes_and_copies() {
        use crate::library::RemixOf;
        let dir = std::env::temp_dir().join(format!("synththing-remix-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.json"), r#"{ "previews": false }"#).unwrap();
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = info(&base).unwrap().key;
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let body: String = (0..30)
            .map(|i| format!("    rect({i}, {}, {} + w / 9, h - {i}, {{ r = {}, g = 40, b = 90 }})\n", i * 3, i * 2, i * 8))
            .collect();
        let original = format!("function render(w, h)\n    clear({{ r = 0, g = 0, b = 0 }})\n{body}end\n");
        let mut bars = upload_of("Bars", "visualizer", &original);
        bars.slug = Some("bars".into());
        let first = upload(&base, &key, &bars, Some(&alice)).unwrap();

        // Bob's take on it: nearly the same.
        let tweaked = original.replacen("g = 40", "g = 200", 2);
        let mut take = upload_of("Bars Again", "visualizer", &tweaked);
        take.slug = Some("bars-again".into());
        let refused = upload(&base, &key, &take, Some(&bob)).unwrap_err();
        assert!(refused.contains("\"Bars\"") && refused.contains("remix"), "{refused}");
        take.remix_of = Some(RemixOf {
            server: String::new(),
            id: first.id.clone(),
            author_id: Some(alice.id()),
            slug: Some("bars".into()),
            sha256: first.sha256.clone(),
            name: "Bars".into(),
        });
        let remix = upload(&base, &key, &take, Some(&bob)).unwrap();
        let original_details = details(&base, &first.id).unwrap();
        assert_eq!(original_details.remixes.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), [remix.id.as_str()]);
        let link = details(&base, &remix.id).unwrap().remix_of.unwrap();
        assert_eq!((link.id.as_str(), link.version, link.gone), (first.id.as_str(), Some(1), false));
        // A remix of the remix is fine too (it resembles the original).
        let mut again = upload_of("Bars Thrice", "visualizer", &tweaked.replacen("b = 90", "b = 10", 2));
        again.remix_of = Some(RemixOf { id: remix.id.clone(), sha256: remix.sha256.clone(), name: "Bars Again".into(), ..Default::default() });
        assert!(upload(&base, &key, &again, None).is_ok());
        // No changes: refused.
        let mut same = upload_of("Same Bars", "visualizer", &original);
        same.remix_of = Some(RemixOf { id: first.id.clone(), sha256: first.sha256.clone(), name: "Bars".into(), ..Default::default() });
        assert!(upload(&base, &key, &same, None).unwrap_err().contains("change something"));
        // Alice's own copy isn't a copy.
        let mut mine = upload_of("Bars 2", "visualizer", &tweaked);
        mine.slug = Some("bars-2".into());
        assert!(upload(&base, &key, &mine, Some(&alice)).is_ok());

        // A bundled script: refused as is, taken as a remix.
        let disco = crate::lua_visualizer::bundled_default("disco.lua").unwrap().replacen("255", "250", 2);
        let copy = upload_of("My Disco", "visualizer", &disco);
        assert!(upload(&base, &key, &copy, None).unwrap_err().contains("comes with synththing"));
        let credited = Upload { remix_of: crate::library::bundled_original(&disco), ..copy };
        let mine = upload(&base, &key, &credited, None).unwrap();
        // Linked to the official server's disco, found there by author and slug.
        let link = details(&base, &mine.id).unwrap().remix_of.unwrap();
        assert_eq!(link.server, crate::library::OFFICIAL_URL);
        assert_eq!(link.author_id, Some(crate::library::official_id()));
        assert_eq!(link.slug.as_deref(), Some("disco"));

        // The original deleted: the remix says so.
        delete(&base, &key, &first.id, &alice).unwrap();
        assert!(details(&base, &remix.id).unwrap().remix_of.unwrap().gone);
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An authority: emails attached with a code (changing one needs the
    /// current address's too), key-signed rotations, recovery by email
    /// (and cancelling one), and another server taking the records.
    #[test]
    fn rotations_and_recovery() {
        use crate::library::rotation::{EmailConfirm, EmailRequest};
        let dir = std::env::temp_dir().join(format!("synththing-authority-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("server.json"),
            r#"{ "previews": false, "check_uploads": false, "authority": true, "public_url": "http://authority.test",
                 "mail": { "from": "codes@example.org" } }"#,
        )
        .unwrap();
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = info(&base).unwrap().key;
        assert!(info(&base).unwrap().authority);
        // Another server that trusts this authority.
        let other_dir = dir.join("other");
        std::fs::create_dir_all(&other_dir).unwrap();
        std::fs::write(other_dir.join("server.json"), format!(r#"{{ "previews": false, "check_uploads": false, "authorities": ["{key}"] }}"#))
            .unwrap();
        let other = server::start(&other_dir, Some("127.0.0.1:0".into())).unwrap();
        let other_base = format!("http://{}", other.addr);
        let other_key = info(&other_base).unwrap().key;
        let code_in = |mail: &crate::library::server::Mail| mail.subject.rsplit(' ').next().unwrap().to_string();

        let alice = Identity::generate().unwrap();
        let mut bars = upload_of("Bars", "visualizer", "function render() end");
        bars.slug = Some("bars".into());
        let here = upload(&base, &key, &bars, Some(&alice)).unwrap();
        let there = upload(&other_base, &other_key, &bars, Some(&alice)).unwrap();

        // An email, attached with its code.
        assert!(!identity_status(&base, &key, &alice).unwrap().email);
        let ask = EmailRequest { address: Some("Alice@Example.org".into()), current: None };
        assert_eq!(email(&base, &key, &alice, &ask).unwrap(), ["new"]);
        let mail = running.outbox().last().unwrap().clone();
        assert_eq!(mail.to, "Alice@Example.org");
        assert!(!email_confirm(&base, &key, &alice, &EmailConfirm { code: Some("WRONG-CODE".into()), ..Default::default() })
            .is_ok_and(|e| e));
        assert!(email_confirm(&base, &key, &alice, &EmailConfirm { code: Some(code_in(&mail)), ..Default::default() }).unwrap());
        // Changing it needs the current address.
        let change = EmailRequest { address: Some("new@example.org".into()), current: Some("wrong@example.org".into()) };
        assert!(email(&base, &key, &alice, &change).unwrap_err().contains("isn't the address"));
        // A change cancelled from the current address's email: changes pause.
        let change = EmailRequest { address: Some("new@example.org".into()), current: Some("alice@example.org".into()) };
        assert_eq!(email(&base, &key, &alice, &change).unwrap(), ["current", "new"]);
        let to_current = running.outbox().into_iter().find(|m| m.to == "alice@example.org").unwrap();
        assert!(to_current.text.contains("pauses changes to your identity's email"));
        let link = to_current.text.lines().find(|l| l.contains("cancel?token=")).unwrap().trim().to_string();
        let path = link.trim_start_matches("http://authority.test");
        let page = agent().get(format!("{base}{path}")).call().unwrap().body_mut().read_to_string().unwrap();
        assert!(page.contains("stays as it is"), "{page}");
        assert!(email(&base, &key, &alice, &change).unwrap_err().contains("paused"));

        // A key-signed rotation: the old key is refused, the new one owns the script.
        let alice2 = Identity::generate().unwrap();
        let first = rotate(&base, &key, &alice, &alice2).unwrap();
        assert_eq!((first.how.as_str(), first.new.as_str()), ("key", alice2.public().as_str()));
        assert!(identity_status(&base, &key, &alice).unwrap_err().contains("replaced"));
        assert_eq!(details(&base, &here.id).unwrap().summary.author_id, Some(alice2.id()));
        assert!(identity_status(&base, &key, &alice2).unwrap().email, "the email came along");

        // Lost: recovered by email, to a third key.
        let alice3 = Identity::generate().unwrap();
        recover(&base, &key, " alice@example.org ", &alice3).unwrap();
        let mail = running.outbox().last().unwrap().clone();
        assert!(mail.text.contains("http://authority.test/api/v1/identity/cancel?token="));
        // (An address that isn't attached: the same answer, no email.)
        let sent = running.outbox().len();
        recover(&base, &key, "nobody@example.org", &Identity::generate().unwrap()).unwrap();
        assert_eq!(running.outbox().len(), sent);
        let second = recover_confirm(&base, &key, &alice3, &code_in(&mail)).unwrap();
        assert_eq!((second.old.as_str(), second.how.as_str()), (alice2.public().as_str(), "email"));
        assert_eq!(details(&base, &here.id).unwrap().summary.author_id, Some(alice3.id()));

        // The whole history, from the authority.
        assert_eq!(lineage(&base, &key, &alice3.public()).unwrap(), [first.clone(), second.clone()]);
        // The other server takes the chain: alice3 owns it there too.
        assert_eq!(apply(&other_base, &[first.clone(), second.clone()]).unwrap(), 2);
        assert_eq!(apply(&other_base, &[first, second]).unwrap(), 0, "once");
        assert_eq!(details(&other_base, &there.id).unwrap().summary.author_id, Some(alice3.id()));
        bars.source = "function render() end -- 2".into();
        assert!(upload(&other_base, &other_key, &bars, Some(&alice)).unwrap_err().contains("replaced"));
        assert_eq!(upload(&other_base, &other_key, &bars, Some(&alice3)).unwrap().version, 2);
        // A record from an authority it doesn't trust: refused.
        let mut forged = rotate(&base, &key, &alice3, &Identity::generate().unwrap()).unwrap();
        forged.authority = other_key.clone();
        assert!(apply(&other_base, &[forged]).unwrap_err().contains("trusts"));

        // "That wasn't me": cancelled, and recovery paused for a day.
        let alice5 = Identity::generate().unwrap();
        recover(&base, &key, "alice@example.org", &alice5).unwrap();
        let mail = running.outbox().last().unwrap().clone();
        let link = mail.text.lines().find(|l| l.contains("cancel?token=")).unwrap().trim().to_string();
        assert!(mail.text.contains("pauses recovery"));
        let path = link.trim_start_matches("http://authority.test");
        let page = agent().get(format!("{base}{path}")).call().unwrap().body_mut().read_to_string().unwrap();
        assert!(page.contains("Cancelled"), "{page}");
        assert!(recover_confirm(&base, &key, &alice5, &code_in(&mail)).is_err());
        // (Paused: the usual answer, but no email.)
        let sent = running.outbox().len();
        recover(&base, &key, "alice@example.org", &Identity::generate().unwrap()).unwrap();
        assert_eq!(running.outbox().len(), sent);
        running.stop();
        other.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting an identity's data: gone from listings at once, the key
    /// refused, all of it back with a restore; with an email attached on a
    /// server with mail, only with a code sent to it.
    #[test]
    fn identities_are_deleted_and_restored() {
        use crate::library::rotation::{EmailConfirm, EmailRequest};
        let dir = std::env::temp_dir().join(format!("synththing-deletion-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("server.json"),
            r#"{ "previews": false, "check_uploads": false, "authority": true, "public_url": "http://authority.test",
                 "mail": { "from": "codes@example.org" } }"#,
        )
        .unwrap();
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = info(&base).unwrap().key;
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let mut mine = upload_of("Mine", "visualizer", "function render() end");
        mine.slug = Some("mine".into());
        let mine = upload(&base, &key, &mine, Some(&alice)).unwrap();
        let mut theirs = upload_of("Theirs", "visualizer", "function render() end -- theirs");
        theirs.slug = Some("theirs".into());
        let theirs = upload(&base, &key, &theirs, Some(&bob)).unwrap();
        encore(&base, &key, &theirs.id, &alice, true).unwrap();

        // No email: deleted at once (for 30 days).
        let deleted = delete_identity(&base, &key, &alice, None).unwrap();
        assert!(deleted.deleted.is_some_and(|t| t > 29 * 24 * 3600) && !deleted.sent);
        assert!(details(&base, &mine.id).is_err());
        assert_eq!(list(&base, &search("", None, "new")).unwrap().total, 1);
        assert_eq!(details(&base, &theirs.id).unwrap().summary.encores, 0);
        assert!(encore(&base, &key, &theirs.id, &alice, false).unwrap_err().contains("being deleted"));
        // Restored: all of it back.
        assert_eq!(restore_identity(&base, &key, &alice).unwrap(), 1);
        assert!(details(&base, &mine.id).is_ok());
        assert_eq!(details(&base, &theirs.id).unwrap().summary.encores, 1);
        assert!(restore_identity(&base, &key, &alice).unwrap_err().contains("nothing"));

        // With an email attached: the address, then its code.
        email(&base, &key, &alice, &EmailRequest { address: Some("alice@example.org".into()), current: None }).unwrap();
        let code = |running: &server::Running| running.outbox().last().unwrap().subject.rsplit(' ').next().unwrap().to_string();
        email_confirm(&base, &key, &alice, &EmailConfirm { code: Some(code(&running)), ..Default::default() }).unwrap();
        assert!(delete_identity(&base, &key, &alice, None).unwrap_err().contains("email attached"));
        assert!(delete_identity(&base, &key, &alice, Some("wrong@example.org")).unwrap_err().contains("email attached"));
        assert!(delete_identity(&base, &key, &alice, Some("Alice@Example.org")).unwrap().sent);
        let mail = running.outbox().last().unwrap().clone();
        assert!(mail.text.contains("to delete your identity's data") && mail.text.contains("cancel?token="));
        assert!(details(&base, &mine.id).is_ok(), "not until the code");
        assert!(delete_confirm(&base, &key, &alice, &code(&running)).unwrap().deleted.is_some());
        assert!(details(&base, &mine.id).is_err());

        // 30 days on: gone for good, the email too; the key's new here.
        running.purge_as_if_later();
        assert!(restore_identity(&base, &key, &alice).unwrap_err().contains("nothing"));
        assert!(!identity_status(&base, &key, &alice).unwrap().email);
        assert_eq!(details(&base, &theirs.id).unwrap().summary.encores, 0);
        let again = upload_of("Mine", "visualizer", "function render() end -- again");
        assert!(upload(&base, &key, &Upload { slug: Some("mine".into()), ..again }, Some(&alice)).is_ok());
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Notifications: milestones, moderation, remixes, reports for the
    /// moderators and back to their reporters; pushed over the stream as
    /// they happen, listed, marked read.
    #[test]
    fn notifications_are_kept_and_pushed() {
        use crate::library::{AdminAction, ReportRequest};
        let dir = std::env::temp_dir().join(format!("synththing-notifications-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.json"), r#"{ "previews": false, "check_uploads": false }"#).unwrap();
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = info(&base).unwrap().key;
        let server_id = Identity::from_secret_hex(&std::fs::read_to_string(dir.join("server.key")).unwrap()).unwrap();
        let (alice, bob, mod_) = (Identity::generate().unwrap(), Identity::generate().unwrap(), Identity::generate().unwrap());
        let mut bars = upload_of("Bars", "visualizer", "function render() end");
        bars.slug = Some("bars".into());
        let bars = upload(&base, &key, &bars, Some(&alice)).unwrap();
        // (The moderator needs to have been seen, to be made one.)
        encore(&base, &key, &bars.id, &mod_, true).unwrap();
        admin::<serde_json::Value>(&base, &key, &server_id, &AdminAction::AddAdmin { key: mod_.public() }).unwrap();

        // Alice listens.
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let (base, key, alice) = (base.clone(), key.clone(), alice.clone());
            std::thread::spawn(move || {
                let _ = notification_stream(&base, &key, &alice, |n| tx.send(n).is_ok());
            });
        }
        std::thread::sleep(Duration::from_millis(300));

        // Ten encores: a milestone, pushed.
        for _ in 0..9 {
            encore(&base, &key, &bars.id, &Identity::generate().unwrap(), true).unwrap();
        }
        let pushed = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((pushed.kind.as_str(), pushed.data["count"].as_u64()), ("encore_milestone", Some(10)));
        assert_eq!(pushed.data["name"], "Bars");
        // Ten downloads: another.
        for _ in 0..10 {
            download(&base, &bars.id, None, &key).unwrap();
        }
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().kind, "download_milestone");

        // Bob reports it: the moderator hears; it's hidden and resolved:
        // alice hears, then bob does.
        let complaint = ReportRequest { reason: "spam".into(), version: 1, ..Default::default() };
        let number = report(&base, &key, &bars.id, &bob, &complaint).unwrap();
        let theirs = notifications(&base, &key, &mod_, None).unwrap();
        assert_eq!((theirs[0].kind.as_str(), theirs[0].data["report"].as_i64()), ("new_report", Some(number)));
        admin::<serde_json::Value>(&base, &key, &mod_, &AdminAction::Hide { script: bars.id.clone(), reason: "spam".into() })
            .unwrap();
        let hidden = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((hidden.kind.as_str(), hidden.data["reason"].as_str()), ("hidden", Some("spam")));
        admin::<serde_json::Value>(&base, &key, &mod_, &AdminAction::Resolve { report: number, note: "hidden".into() })
            .unwrap();
        let bobs = notifications(&base, &key, &bob, None).unwrap();
        assert_eq!((bobs[0].kind.as_str(), bobs[0].data["outcome"].as_str()), ("report_handled", Some("hidden")));
        admin::<serde_json::Value>(&base, &key, &mod_, &AdminAction::Unhide { script: bars.id.clone() }).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().kind, "reinstated");

        // A remix of it: alice hears who.
        let mut take = upload_of("Bars Again", "visualizer", "function render() clear({ r = 1, g = 2, b = 3 }) end");
        take.slug = Some("bars-again".into());
        take.remix_of = Some(crate::library::RemixOf { id: bars.id.clone(), name: "Bars".into(), ..Default::default() });
        upload(&base, &key, &take, Some(&bob)).unwrap();
        let remixed = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((remixed.kind.as_str(), remixed.data["remix_name"].as_str()), ("remixed", Some("Bars Again")));

        // Listed, newest first, unread until marked.
        let all = notifications(&base, &key, &alice, None).unwrap();
        assert_eq!(
            all.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>(),
            ["remixed", "reinstated", "hidden", "download_milestone", "encore_milestone"]
        );
        assert!(all.iter().all(|n| !n.read) && all[0].template.contains("{remix_name}"));
        assert_eq!(notifications(&base, &key, &alice, Some(all[1].id)).unwrap().len(), 1, "after");
        mark_notifications(&base, &key, &alice, &[all[0].id], false, false).unwrap();
        assert!(notifications(&base, &key, &alice, None).unwrap()[0].read);
        mark_notifications(&base, &key, &alice, &[], true, false).unwrap();
        assert!(notifications(&base, &key, &alice, None).unwrap().iter().all(|n| n.read));
        // Nobody else's.
        assert!(mark_notifications(&base, &key, &bob, &[all[2].id], false, false).is_ok());
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A server's publisher is followed through its rotations: its new key
    /// isn't limited either, owns the scripts, and the server says so.
    #[test]
    fn the_publisher_rotates() {
        let dir = std::env::temp_dir().join(format!("synththing-publisher-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let publisher = Identity::generate().unwrap();
        std::fs::write(
            dir.join("server.json"),
            format!(
                r#"{{ "previews": false, "check_uploads": false, "authority": true, "uploads_per_day": 1,
                     "unlimited_keys": [], "publisher": "{}" }}"#,
                publisher.public()
            ),
        )
        .unwrap();
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = info(&base).unwrap().key;
        let publish = |who: &Identity, n: u32| {
            let mut script = upload_of("Disco", "visualizer", &format!("function render() end -- {n}"));
            script.slug = Some("disco".into());
            upload(&base, &key, &script, Some(who))
        };
        for n in 0..3 {
            assert!(publish(&publisher, n).is_ok(), "the publisher isn't limited");
        }
        assert!(info(&base).unwrap().publisher_rotations.is_empty());
        let next = Identity::generate().unwrap();
        let record = rotate(&base, &key, &publisher, &next).unwrap();
        assert_eq!(info(&base).unwrap().publisher_rotations, [record]);
        assert!(publish(&publisher, 9).unwrap_err().contains("replaced"));
        for n in 10..13 {
            assert_eq!(publish(&next, n).unwrap().version, n - 6, "the new key carries on, unlimited");
        }
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Encores (not your own; given and taken back), reports (one open
    /// per key), and admins: the server's own key, keys it makes admins,
    /// hiding, resolving, banning.
    #[test]
    fn encores_reports_and_moderation() {
        use crate::library::{AdminAction, AdminScriptInfo, Ban, Report, ReportRequest};
        let dir = std::env::temp_dir().join(format!("synththing-moderation-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.json"), r#"{ "previews": false }"#).unwrap();
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = info(&base).unwrap().key;
        let server_id = Identity::from_secret_hex(&std::fs::read_to_string(dir.join("server.key")).unwrap()).unwrap();
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let mut bars = upload_of("Bars", "visualizer", "function render() end");
        bars.slug = Some("bars".into());
        let id = upload(&base, &key, &bars, Some(&alice)).unwrap().id;

        // Encores.
        assert!(encore(&base, &key, &id, &alice, true).unwrap_err().contains("own"));
        assert_eq!(encore(&base, &key, &id, &bob, true).unwrap(), EncoreState { encores: 1, encored: true });
        assert_eq!(encore(&base, &key, &id, &bob, true).unwrap().encores, 1, "once per key");
        assert_eq!(details_as(&base, &id, Some(&bob.public())).unwrap().encored, Some(true));
        assert_eq!(details_as(&base, &id, Some(&alice.public())).unwrap().encored, Some(false));
        assert_eq!(details(&base, &id).unwrap().summary.encores, 1);
        assert_eq!(encore(&base, &key, &id, &bob, false).unwrap(), EncoreState { encores: 0, encored: false });

        // Reports.
        let complaint = ReportRequest {
            reason: "derogatory".into(),
            details: "line 2 says rude things".into(),
            version: 1,
            lines: vec![(2, 3)],
            sprites: vec![5],
        };
        let number = report(&base, &key, &id, &bob, &complaint).unwrap();
        assert!(report(&base, &key, &id, &bob, &complaint).unwrap_err().contains("already"));
        let made_up = ReportRequest { reason: "vibes".into(), ..complaint.clone() };
        assert!(report(&base, &key, &id, &alice, &made_up).unwrap_err().contains("reason"));

        // Admins: not bob, until the server's key makes him one.
        let as_server = |action: AdminAction| admin::<serde_json::Value>(&base, &key, &server_id, &action);
        assert!(admin::<serde_json::Value>(&base, &key, &bob, &AdminAction::Whoami).unwrap_err().contains("admins"));
        let reports: Vec<Report> = admin(&base, &key, &server_id, &AdminAction::Reports { all: false }).unwrap();
        assert_eq!((reports.len(), reports[0].id), (1, number));
        assert_eq!(reports[0].request, complaint);
        assert_eq!(reports[0].reporter_id, bob.id());
        as_server(AdminAction::AddAdmin { key: format!("#{}", bob.id()) }).unwrap();
        assert!(admin::<serde_json::Value>(&base, &key, &bob, &AdminAction::Whoami).is_ok());

        // Hidden: gone from listings and downloads, still there for admins.
        admin::<serde_json::Value>(&base, &key, &bob, &AdminAction::Hide { script: id.clone(), reason: "rude".into() })
            .unwrap();
        assert!(details(&base, &id).is_err());
        assert_eq!(list(&base, &search("", None, "new")).unwrap().total, 0);
        let info: AdminScriptInfo = admin(&base, &key, &bob, &AdminAction::Info { script: id.clone() }).unwrap();
        assert!(info.hidden && info.reports[0].hidden && info.author_key == Some(alice.public()));
        assert_eq!(info.uploads[0].1.as_deref(), Some("127.0.0.1"));
        let source: serde_json::Value =
            admin(&base, &key, &bob, &AdminAction::Source { script: id.clone(), version: None }).unwrap();
        assert_eq!(source["source"], "function render() end");
        as_server(AdminAction::Unhide { script: id.clone() }).unwrap();
        assert!(details(&base, &id).is_ok());
        as_server(AdminAction::Resolve { report: number, note: "talked to them".into() }).unwrap();
        let open: Vec<Report> = admin(&base, &key, &server_id, &AdminAction::Reports { all: false }).unwrap();
        assert!(open.is_empty());

        // Banned (by ID), with their scripts hidden: no uploads, encores
        // or reports, until unbanned.
        let banned = as_server(AdminAction::Ban {
            target: alice.id(),
            hours: Some(1),
            reason: "spam".into(),
            hide_scripts: true,
            ban_addresses: false,
        })
        .unwrap();
        assert_eq!(banned["addresses_banned"], 0);
        let bans: Vec<Ban> = admin(&base, &key, &server_id, &AdminAction::Bans).unwrap();
        assert_eq!(bans[0].target, alice.public());
        assert!(details(&base, &id).is_err());
        bars.source = "function render() end -- 2".into();
        assert!(upload(&base, &key, &bars, Some(&alice)).unwrap_err().contains("banned"));
        as_server(AdminAction::Unban { target: alice.public() }).unwrap();
        assert!(upload(&base, &key, &bars, Some(&alice)).is_ok());

        // Banned with her addresses: the one she uploaded from is banned
        // too (for 30 days), so a new key from there gets nowhere either.
        let banned = as_server(AdminAction::Ban {
            target: alice.public(),
            hours: None,
            reason: "evading".into(),
            hide_scripts: false,
            ban_addresses: true,
        })
        .unwrap();
        assert_eq!(banned["addresses_banned"], 1);
        let bans: Vec<Ban> = admin(&base, &key, &server_id, &AdminAction::Bans).unwrap();
        let address = bans.iter().find(|b| b.target == "127.0.0.1").unwrap();
        let month = 30 * 24 * 3600;
        assert!(address.until.is_some_and(|u| (u - address.created - month).abs() < 60), "{address:?}");
        let carol = Identity::generate().unwrap();
        assert!(encore(&base, &key, &id, &carol, true).unwrap_err().contains("banned"));
        as_server(AdminAction::Unban { target: "127.0.0.1".into() }).unwrap();
        // She comes back: the address is banned again, from her.
        assert!(encore(&base, &key, &id, &alice, true).unwrap_err().contains("banned"));
        let bans: Vec<Ban> = admin(&base, &key, &server_id, &AdminAction::Bans).unwrap();
        assert!(bans.iter().any(|b| b.target == "127.0.0.1" && b.reason.contains("seen with the banned key")));
        as_server(AdminAction::Unban { target: "127.0.0.1".into() }).unwrap();
        as_server(AdminAction::Unban { target: alice.public() }).unwrap();
        as_server(AdminAction::Delete { script: id.clone() }).unwrap();
        let info = as_server(AdminAction::Info { script: id.clone() });
        assert!(info.unwrap_err().contains("no such script"));
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signed_uploads_belong_to_their_key() {
        let dir = std::env::temp_dir().join(format!("synththing-signed-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.json"), r#"{ "mode": "signed" }"#).unwrap();
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = info(&base).unwrap().key;
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let mut bars = upload_of("Bars", "visualizer", "function render() end");
        bars.slug = Some("bars".into());

        assert!(upload(&base, &key, &bars, None).unwrap_err().contains("signed"), "signed only");
        let first = upload(&base, &key, &bars, Some(&alice)).unwrap();
        assert_eq!(first.version, 1);
        // The same source again: nothing new.
        assert_eq!(upload(&base, &key, &bars, Some(&alice)).unwrap().version, 1);
        bars.source = "function render() clear({r=9,g=9,b=9}) end".into();
        bars.name = "Bars Deluxe".into();
        let second = upload(&base, &key, &bars, Some(&alice)).unwrap();
        assert_eq!((second.id.as_str(), second.version), (first.id.as_str(), 2));
        // Another key's "bars" is another script.
        let theirs = upload(&base, &key, &bars, Some(&bob)).unwrap();
        assert_ne!(theirs.id, first.id);

        let details = details(&base, &first.id).unwrap();
        assert_eq!(details.summary.name, "Bars Deluxe");
        assert_eq!(details.summary.author_id, Some(alice.id()));
        assert_eq!(details.summary.slug.as_deref(), Some("bars"));
        assert_eq!(details.versions.len(), 2);
        assert_eq!(details.versions[1].min_app_version, "0.1.1");
        // A version needing a newer app than this one is skipped.
        bars.source = "function render() local r = recording() end".into();
        let needs_new = upload(&base, &key, &bars, Some(&alice)).unwrap();
        let details = super::details(&base, &first.id).unwrap();
        assert_eq!(details.versions[2].min_app_version, "0.4.0");
        assert_eq!(details.newest_for("0.3.3").map(|v| v.version), Some(2));
        assert_eq!(details.newest_for("0.4.0").map(|v| v.version), Some(needs_new.version));
        // A publisher that knows better (a newer app) raises it; a lower
        // claim doesn't lower it.
        bars.source = "function render() local r = recording() end -- 2".into();
        bars.min_app_version = Some("9.0.0".into());
        let claimed = upload(&base, &key, &bars, Some(&alice)).unwrap();
        bars.source = "function render() local r = recording() end -- 3".into();
        bars.min_app_version = Some("0.1.1".into());
        upload(&base, &key, &bars, Some(&alice)).unwrap();
        let details = super::details(&base, &first.id).unwrap();
        let min = |v: u32| details.versions.iter().find(|x| x.version == v).unwrap().min_app_version.clone();
        assert_eq!((min(claimed.version), min(claimed.version + 1)), ("9.0.0".to_string(), "0.4.0".to_string()));
        assert_eq!(download(&base, &first.id, Some(1), &key).unwrap().source, "function render() end");
        assert_eq!(by_slug(&base, &alice.id(), "bars").unwrap().map(|s| s.id), Some(first.id.clone()));
        assert_eq!(by_slug(&base, &alice.id(), "nope").unwrap(), None);
        let alices = Search { author: Some(alice.id()), sort: "new", ..Default::default() };
        assert_eq!(list(&base, &alices).unwrap().total, 1);
        assert_eq!(user(&base, &alice.id()).unwrap().names, ["tester"]);

        assert!(delete(&base, &key, &first.id, &bob).unwrap_err().contains("only its author"));
        delete(&base, &key, &first.id, &alice).unwrap();
        assert!(super::details(&base, &first.id).is_err());
        assert!(user(&base, &alice.id()).is_err(), "nothing left by them");
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn urls() {
        assert_eq!(normalize_url(" synththing.p51.nl/ "), "https://synththing.p51.nl");
        assert_eq!(normalize_url("http://localhost:7381"), "http://localhost:7381");
    }
}
