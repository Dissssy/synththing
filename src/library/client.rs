//! The app's side of a library server: one function per request, blocking
//! (the app runs them on threads of its own). Errors come back as a line
//! to show: the server's own message when it sent one.
//!
//! A server's key is pinned by the app the first time it's seen (see
//! `Info::key`); sources are only accepted signed by that key.

use std::sync::OnceLock;
use std::time::Duration;

use ureq::Agent;

use super::{
    source_message, verify_hex, ApiError, Info, Listing, Receipt, ScriptDetails, Upload, SHA256_HEADER,
    SIGNATURE_HEADER, VERSION_HEADER,
};

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

/// A page of scripts: `query` searched for, in `category` (or all),
/// sorted by `"new"` or `"top"`.
pub fn list(base: &str, query: &str, category: Option<&str>, sort: &str, page: u32) -> Result<Listing, String> {
    let mut request = agent().get(format!("{base}/api/v1/scripts")).query("sort", sort).query("page", page.to_string());
    if !query.trim().is_empty() {
        request = request.query("q", query.trim());
    }
    if let Some(category) = category {
        request = request.query("category", category);
    }
    read(check(request.call())?)
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

pub fn upload(base: &str, upload: &Upload) -> Result<Receipt, String> {
    read(check(agent().post(format!("{base}/api/v1/scripts")).send_json(upload))?)
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
        }
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

        let bars = upload(&base, &upload_of("Neon Bars", "visualizer", "function render() clear({r=1,g=2,b=3}) end")).unwrap();
        let game = upload(&base, &upload_of("Space Game", "game", "function render() end -- pew")).unwrap();
        assert_eq!(bars.version, 1);
        assert_ne!(bars.id, game.id);
        // Refused: not a script.
        let refused = upload(&base, &upload_of("Nope", "visualizer", "print('hi')")).unwrap_err();
        assert!(refused.contains("render"), "{refused}");

        let all = list(&base, "", None, "new", 0).unwrap();
        assert_eq!(all.total, 2);
        assert_eq!(all.scripts[0].name, "Space Game", "newest first");
        assert_eq!(list(&base, "neo", None, "new", 0).unwrap().scripts[0].id, bars.id, "prefix search");
        assert_eq!(list(&base, "\"pew", None, "new", 0).unwrap().total, 0, "quotes are just dropped");
        assert_eq!(list(&base, "", Some("game"), "top", 0).unwrap().scripts[0].id, game.id);
        assert_eq!(list(&base, "things", Some("toy"), "new", 0).unwrap().total, 0);

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
        assert_eq!(list(&base, "", None, "new", 0).unwrap().total, 2);
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn urls() {
        assert_eq!(normalize_url(" synththing.p51.nl/ "), "https://synththing.p51.nl");
        assert_eq!(normalize_url("http://localhost:7381"), "http://localhost:7381");
    }
}
