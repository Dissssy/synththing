//! `POST /api/v1/reach`, signed: whether a hub's address works from the
//! open internet (`library/reach.rs`). The server fetches the hub's
//! `hello` itself, only from public addresses it resolved and checked (the
//! connection goes to that very address, so the name can't be pointed
//! elsewhere meanwhile), with no redirects, and within limits, so it
//! can't be used to scan anyone.
//!
//! Once an address has passed, the server remembers it (the `hubs` table)
//! and checks it again itself when no one has for 6 hours, so it notices
//! a hub that's down and can't ask. After 3 failed checks in a row it
//! notifies the identity (`hub_unreachable`), and again once one passes
//! (`hub_reachable`). A hub that hasn't asked for 30 days is forgotten, as
//! is the address of a hub replaced by another.

use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

use rusqlite::{params, OptionalExtension};
use tiny_http::Request;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use super::{client_ip, error, json, moderation, notifications, now, read_body_or, signer, Reply, State};
use crate::library::reach::{self, Ask, Probe, Reach, Verdict};

const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const CONNECT: Duration = Duration::from_secs(5);
const ALL: Duration = Duration::from_secs(10);
/// The most of a `hello` read.
const MOST: u64 = 4096;
/// How long a remembered address goes unchecked before the server checks
/// it itself, and how often it looks for those due.
const RECHECK: i64 = 6 * 60 * 60;
pub(super) const RECHECK_LOOK: Duration = Duration::from_secs(10 * 60);
/// Failed checks in a row before the identity hears of it.
const WARN_AFTER: i64 = 3;
/// A remembered address whose hub hasn't asked this long is forgotten.
const FORGET: i64 = 30 * 24 * 60 * 60;

pub(super) const SCHEMA_V20: &str = "
BEGIN;
CREATE TABLE hubs (
    key TEXT NOT NULL,              -- whose
    url TEXT NOT NULL,
    hub_key TEXT NOT NULL,
    checked INTEGER NOT NULL,       -- the last check
    asked INTEGER NOT NULL,         -- the last check someone asked for
    failures INTEGER NOT NULL DEFAULT 0,  -- in a row
    warned INTEGER NOT NULL DEFAULT 0,    -- hub_unreachable was sent
    PRIMARY KEY (key, url)
);
PRAGMA user_version = 20;
COMMIT;
";

pub(super) fn reach(state: &State, request: &mut Request) -> Reply {
    if !state.config.sync {
        return error(404, "this server doesn't sync");
    }
    let body = match read_body_or(request, 4096, "that's too long an address") {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let key = match signer(state, request, "POST", "/api/v1/reach", &body) {
        Ok(Some(key)) => key,
        Ok(None) => return error(401, "that needs your signature"),
        Err(reply) => return reply,
    };
    let ask: Ask = match serde_json::from_slice(&body) {
        Ok(ask) => ask,
        Err(e) => return error(400, format!("not understood: {e}")),
    };
    let Some(base) = base(&ask.url) else {
        return error(400, "that isn't an http:// or https:// address");
    };
    let host = reach::host(&base).to_ascii_lowercase();
    for (bucket, by, most, window) in [("reach", key.as_str(), 6, HOUR), ("reach-day", key.as_str(), 30, DAY), ("reach-host", host.as_str(), 10, HOUR)] {
        if let Err(reply) = moderation::limit(state, bucket, by, most, window, "address checks") {
            return reply;
        }
    }
    let found = probe(&base, &ask.hub_key, client_ip(state, request), reach::public);
    if let Err(e) = remember(state, &key, &base, &ask.hub_key, &found, true) {
        log::warn!("remembering a hub's address: {e}");
    }
    json(200, &found)
}

/// A remembered address, as the `hubs` table has it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Standing {
    failures: i64,
    warned: bool,
}

/// What a check makes of an address's standing (`None`: not remembered),
/// and what to tell the identity.
fn after(standing: Option<Standing>, passed: bool) -> (Option<Standing>, Option<&'static str>) {
    match (standing, passed) {
        (Some(Standing { warned: true, .. }), true) => (Some(Standing { failures: 0, warned: false }), Some("hub_reachable")),
        (_, true) => (Some(Standing { failures: 0, warned: false }), None),
        (None, false) => (None, None),
        (Some(Standing { failures, warned }), false) => {
            let failures = failures + 1;
            let warn = !warned && failures >= WARN_AFTER;
            (Some(Standing { failures, warned: warned || warn }), warn.then_some("hub_unreachable"))
        }
    }
}

/// Note a check of `url` (the hub with `hub_key`, `key`'s) in the `hubs`
/// table, telling `key` once it's failed 3 times in a row, and once it
/// passes again. `asked`: someone asked for it (not the server's own).
fn remember(state: &State, key: &str, url: &str, hub_key: &str, found: &Reach, asked: bool) -> rusqlite::Result<()> {
    let db = state.db();
    let row: Option<(String, i64, bool)> = db
        .query_row("SELECT hub_key, failures, warned FROM hubs WHERE key = ? AND url = ?", params![key, url], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    // (Another hub's address, or an old one of this hub's: only a pass
    // takes its place.)
    let standing = row.filter(|(had, ..)| had == hub_key).map(|(_, failures, warned)| Standing { failures, warned });
    let (next, tell) = after(standing, found.reachable());
    let Some(next) = next else { return Ok(()) };
    let now = now();
    if standing.is_none() {
        db.execute("DELETE FROM hubs WHERE key = ? AND hub_key != ?", params![key, hub_key])?;
    }
    db.execute(
        "INSERT INTO hubs (key, url, hub_key, checked, asked, failures, warned) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)
         ON CONFLICT (key, url) DO UPDATE SET hub_key = ?3, checked = ?4, asked = CASE WHEN ?7 THEN ?4 ELSE asked END, failures = ?5, warned = ?6",
        params![key, url, hub_key, now, next.failures, next.warned, asked],
    )?;
    if let Some(kind) = tell {
        let hint = reach::hint(url, found).unwrap_or_default();
        notifications::notify(state, &db, key, kind, serde_json::json!({ "url": url, "hint": hint }), false);
    }
    Ok(())
}

/// Check each remembered address no one has for 6 hours, and forget those
/// whose hub hasn't asked for 30 days.
pub(super) fn recheck(state: &State) {
    let due: rusqlite::Result<Vec<(String, String, String)>> = (|| {
        let db = state.db();
        db.execute("DELETE FROM hubs WHERE asked < ?", [now() - FORGET])?;
        db.prepare("SELECT key, url, hub_key FROM hubs WHERE checked < ?")?.query_map([now() - RECHECK], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect()
    })();
    for (key, url, hub_key) in due.unwrap_or_default() {
        let found = probe(&url, &hub_key, String::new(), reach::public);
        if let Err(e) = remember(state, &key, &url, &hub_key, &found, false) {
            log::warn!("rechecking a hub's address: {e}");
        }
    }
}

/// `url` as `scheme://authority`, if it's a plain http(s) address.
fn base(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let (scheme, rest) = url.split_once("://")?;
    let ok = matches!(scheme, "http" | "https") && !rest.is_empty() && !rest.contains(['/', '?', '#', '@', ' ']);
    ok.then(|| format!("{scheme}://{rest}"))
}

/// What the hub with `hub_key` at `base` answers from here, at the first
/// IPv4 and IPv6 address its name has, trying only those `allowed`.
pub(super) fn probe(base: &str, hub_key: &str, from: String, allowed: fn(IpAddr) -> bool) -> Reach {
    let host = reach::host(base);
    let default = if base.starts_with("https://") { 443 } else { 80 };
    let port = base.rsplit_once(':').and_then(|(_, port)| port.parse().ok()).unwrap_or(default);
    let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
        Ok(addrs) => addrs.collect(),
        Err(e) => return Reach { dns: Some(e.to_string()), v4: None, v6: None, from },
    };
    let first = |v6: bool| addrs.iter().find(|addr| addr.is_ipv6() == v6).map(|addr| try_at(base, hub_key, *addr, allowed));
    Reach { dns: None, v4: first(false), v6: first(true), from }
}

fn try_at(base: &str, hub_key: &str, addr: SocketAddr, allowed: fn(IpAddr) -> bool) -> Probe {
    let verdict = if allowed(addr.ip()) { hello(base, hub_key, addr) } else { Verdict::Private };
    Probe { addr: addr.ip().to_string(), verdict }
}

fn hello(base: &str, hub_key: &str, addr: SocketAddr) -> Verdict {
    let config = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT))
        .timeout_global(Some(ALL))
        .user_agent(format!("synththing/{} (hub check)", crate::build_info::VERSION))
        .build();
    let agent = ureq::Agent::with_parts(config, DefaultConnector::default(), At(addr));
    let challenge = crate::library::new_nonce();
    let started = Instant::now();
    let response = match agent.get(format!("{base}/hub/v1/hello")).query("challenge", &challenge).call() {
        Ok(response) => response,
        Err(e) => return failed(e),
    };
    if response.status() != 200 {
        return Verdict::Status { status: response.status().as_u16() };
    }
    let mut text = String::new();
    if let Err(e) = response.into_body().into_reader().take(MOST).read_to_string(&mut text) {
        return Verdict::Failed { reason: e.to_string() };
    }
    let ms = started.elapsed().as_millis() as u64;
    #[derive(serde::Deserialize)]
    struct Hello {
        key: String,
        signature: String,
    }
    match serde_json::from_str::<Hello>(&text) {
        Ok(hello) if hello.key != hub_key => Verdict::OtherHub,
        Ok(hello) if crate::library::verify_hex(hub_key, &crate::library::hub_hello_message(&challenge), &hello.signature) => Verdict::Ok { ms },
        _ => Verdict::NotAHub,
    }
}

fn failed(e: ureq::Error) -> Verdict {
    match e {
        ureq::Error::Timeout(_) => Verdict::TimedOut,
        ureq::Error::ConnectionFailed => Verdict::Refused,
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Verdict::Refused,
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => Verdict::TimedOut,
        ureq::Error::Tls(reason) => Verdict::Tls { reason: reason.to_string() },
        ureq::Error::Rustls(e) => Verdict::Tls { reason: e.to_string() },
        e => Verdict::Failed { reason: e.to_string() },
    }
}

/// Resolves every name to the one address already checked.
#[derive(Debug)]
struct At(SocketAddr);

impl Resolver for At {
    fn resolve(&self, _: &ureq::http::Uri, _: &ureq::config::Config, _: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut addrs = self.empty();
        addrs.push(self.0);
        Ok(addrs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_failures_in_a_row_warn_once_and_a_pass_says_so() {
        let (mut standing, mut told) = (None, Vec::new());
        assert_eq!(after(None, false), (None, None), "an address never passed isn't remembered");
        for passed in [true, false, false, true, false, false, false, false, true, true] {
            let (next, tell) = after(standing, passed);
            standing = next;
            told.extend(tell);
        }
        assert_eq!(told, ["hub_unreachable", "hub_reachable"]);
        assert_eq!(standing, Some(Standing { failures: 0, warned: false }));
    }

    #[test]
    fn the_server_remembers_a_passing_hub_and_warns_when_it_stops() {
        let dir = std::env::temp_dir().join(format!("synththing-hubs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.json"), r#"{ "previews": false, "check_uploads": false, "sync": true }"#).unwrap();
        let running = super::super::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let state = &running.state;
        let url = "http://127.0.0.1:7390";
        let found = |verdict| Reach { dns: None, v4: Some(Probe { addr: "127.0.0.1".into(), verdict }), v6: None, from: String::new() };
        let kinds = || -> Vec<String> {
            let db = state.db();
            db.prepare("SELECT kind FROM notifications WHERE key = 'me' ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
        };
        let failures = || state.db().query_row("SELECT failures FROM hubs WHERE key = 'me'", [], |r| r.get::<_, i64>(0)).optional().unwrap();

        remember(state, "me", url, "hub", &found(Verdict::Refused), true).unwrap();
        assert_eq!(failures(), None, "not remembered until it passes");
        remember(state, "me", url, "hub", &found(Verdict::Ok { ms: 3 }), true).unwrap();
        assert_eq!(failures(), Some(0));
        // The server's own checks, once it's gone unchecked: it's a private
        // address, so each fails.
        for _ in 0..3 {
            state.db().execute("UPDATE hubs SET checked = 0", []).unwrap();
            recheck(state);
        }
        assert_eq!(failures(), Some(3));
        assert_eq!(kinds(), ["hub_unreachable"]);
        recheck(state);
        assert_eq!(failures(), Some(3), "not due again yet");
        remember(state, "me", url, "hub", &found(Verdict::Ok { ms: 3 }), true).unwrap();
        assert_eq!(kinds(), ["hub_unreachable", "hub_reachable"]);
        // Another hub takes the old one's place; one that hasn't asked for
        // 30 days is forgotten.
        remember(state, "me", "http://127.0.0.2:7390", "new hub", &found(Verdict::Ok { ms: 3 }), true).unwrap();
        assert_eq!(state.db().query_row("SELECT COUNT(*), MIN(hub_key) FROM hubs", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).unwrap(), (1, "new hub".to_string()));
        state.db().execute("UPDATE hubs SET asked = 0", []).unwrap();
        recheck(state);
        assert_eq!(failures(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_bare_web_addresses_are_checked() {
        assert_eq!(base("http://5.161.82.164:7390/").as_deref(), Some("http://5.161.82.164:7390"));
        assert_eq!(base("https://hub.example.org").as_deref(), Some("https://hub.example.org"));
        for url in ["ftp://x.org", "http://", "http://a@b.org", "http://x.org/path", "x.org"] {
            assert_eq!(base(url), None, "{url}");
        }
    }

    #[test]
    fn private_addresses_are_not_tried() {
        let reach = probe("http://127.0.0.1:9", "00", "198.51.100.7".into(), reach::public);
        assert_eq!(reach.v4.map(|probe| probe.verdict), Some(Verdict::Private));
        let reach = probe("http://127.0.0.1:9", "00", String::new(), |_| true);
        assert_eq!(reach.v4.map(|probe| probe.verdict), Some(Verdict::Refused));
    }

    #[test]
    fn the_hub_answers_and_others_dont() {
        use crate::hub_server::HubServer;
        use crate::library::identity::Identity;
        let root = std::env::temp_dir().join(format!("synththing-reach-{}", std::process::id()));
        let hub_key = Identity::generate().unwrap();
        let (server, _came) = HubServer::start(0, Identity::from_secret_hex(&hub_key.secret_hex()).unwrap(), &Identity::generate().unwrap(), [5; 32], root.clone()).unwrap();
        let base = format!("http://127.0.0.1:{}", server.addr().port());
        let verdict = |key: &str| probe(&base, key, String::new(), |_| true).v4.map(|probe| probe.verdict);
        assert!(matches!(verdict(&hub_key.public()), Some(Verdict::Ok { .. })));
        assert_eq!(verdict(&Identity::generate().unwrap().public()), Some(Verdict::OtherHub));

        // (Something else answering, a router's page, say.)
        let page = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let page_base = format!("http://127.0.0.1:{}", page.server_addr().to_ip().unwrap().port());
        std::thread::spawn(move || {
            for request in page.incoming_requests().take(2) {
                let status = if request.url().starts_with("/hub/") { 200 } else { 404 };
                let _ = request.respond(tiny_http::Response::from_string("<html>router</html>").with_status_code(status));
            }
        });
        assert_eq!(probe(&page_base, &hub_key.public(), String::new(), |_| true).v4.map(|probe| probe.verdict), Some(Verdict::NotAHub));
        let _ = std::fs::remove_dir_all(&root);
    }
}
