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
        // The data stays: a restart serves the same scripts with the same key.
        let running = server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        assert_eq!(super::info(&base).unwrap().key, info.key);
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
        as_server(AdminAction::Ban {
            target: alice.id(),
            hours: Some(1),
            reason: "spam".into(),
            hide_scripts: true,
        })
        .unwrap();
        let bans: Vec<Ban> = admin(&base, &key, &server_id, &AdminAction::Bans).unwrap();
        assert_eq!(bans[0].target, alice.public());
        assert!(details(&base, &id).is_err());
        bars.source = "function render() end -- 2".into();
        assert!(upload(&base, &key, &bars, Some(&alice)).unwrap_err().contains("banned"));
        as_server(AdminAction::Unban { target: alice.public() }).unwrap();
        assert!(upload(&base, &key, &bars, Some(&alice)).is_ok());
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
