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
    receipt_message, request_message, source_message, verify_hex, ApiError, Info, Listing, Receipt, ScriptDetails,
    Upload, UserInfo, KEY_HEADER, NONCE_HEADER, SHA256_HEADER, SIGNATURE_HEADER, TIME_HEADER, VERSION_HEADER,
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
    read(check(agent().get(format!("{base}/api/v1/scripts/{id}")).call())?)
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

    /// Signed uploads: a slug used again is a new version, owned by its
    /// key; deletes need it; signed-only servers refuse anonymous uploads.
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
