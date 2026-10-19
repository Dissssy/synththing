//! Signing in another device (`library/pairing.rs`): sealed handoffs,
//! parked by a signed-in app and taken once by the device it's signing
//! in. They're kept in memory only, for `PAIR_SECONDS`: a restart drops
//! them, and the app just shows a new code.

use std::collections::HashMap;

use tiny_http::Request;

use super::moderation::refuse_banned;
use super::{error, header, json, now, read_body_or, signer, Reply, State};
use crate::library::pairing::{Park, PairStatus, Parked, Take, MAX_DEVICE_NAME, MAX_SEALED, PAIR_SECONDS};

/// Handoffs one key may have parked at once.
const MOST_PER_KEY: usize = 5;
/// Handoffs parked at once, from everyone.
const MOST: usize = 10_000;

pub(super) struct Pairing {
    key: String,
    expires: i64,
    /// Until it's taken.
    sealed: Option<String>,
    /// The device that took it.
    taken: Option<String>,
}

pub(super) type Pairings = HashMap<String, Pairing>;

fn pairings(state: &State) -> std::sync::MutexGuard<'_, Pairings> {
    let mut pairings = state.pairings.lock().unwrap_or_else(|p| p.into_inner());
    let now = now();
    pairings.retain(|_, p| p.expires > now);
    pairings
}

/// `POST /api/v1/pair`: park a sealed handoff (signed).
pub(super) fn park(state: &State, request: &mut Request, ip: &str) -> Reply {
    let path = "/api/v1/pair";
    let body = match read_body_or(request, MAX_SEALED + 1024, "the handoff is too big") {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let key = match signer(state, request, "POST", path, &body) {
        Ok(Some(key)) => key,
        Ok(None) => return error(401, "that needs your signature"),
        Err(reply) => return reply,
    };
    if let Err(reply) = refuse_banned(state, Some(&key), ip) {
        return reply;
    }
    let park: Park = match serde_json::from_slice(&body) {
        Ok(park) => park,
        Err(e) => return error(400, format!("not understood: {e}")),
    };
    if park.sealed.is_empty() || park.sealed.len() > MAX_SEALED {
        return error(400, "the handoff is empty or too big");
    }
    let mut pairings = pairings(state);
    if pairings.len() >= MOST || pairings.values().filter(|p| p.key == key).count() >= MOST_PER_KEY {
        return error(429, "too many sign-in codes waiting: wait for one to expire");
    }
    let id = crate::library::new_nonce();
    let expires = now() + PAIR_SECONDS;
    pairings.insert(id.clone(), Pairing { key, expires, sealed: Some(park.sealed), taken: None });
    json(201, &Parked { id, expires })
}

/// `POST /api/v1/pair/{id}/take`: the handoff, once (unsigned: the
/// device taking it has no key yet).
pub(super) fn take(state: &State, request: &mut Request, id: &str) -> Reply {
    let body = match read_body_or(request, 4096, "too big") {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let take: Take = match serde_json::from_slice(&body) {
        Ok(take) => take,
        Err(e) => return error(400, format!("not understood: {e}")),
    };
    let device: String = take.device.trim().chars().take(MAX_DEVICE_NAME).collect();
    let mut pairings = pairings(state);
    match pairings.get_mut(id).and_then(|p| p.sealed.take().map(|sealed| (p, sealed))) {
        Some((pairing, sealed)) => {
            pairing.taken = Some(if device.is_empty() { "another device".into() } else { device });
            json(200, &serde_json::json!({ "sealed": sealed }))
        }
        None => error(404, "that sign-in code was used or has expired: show a new one"),
    }
}

/// `POST /api/v1/pair/{id}`: whether it's been taken (signed by the key
/// that parked it).
pub(super) fn status(state: &State, request: &mut Request, id: &str) -> Reply {
    let path = format!("/api/v1/pair/{id}");
    let body = match read_body_or(request, 1024, "too big") {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let key = match signer(state, request, "POST", &path, &body) {
        Ok(Some(key)) => key,
        Ok(None) => return error(401, "that needs your signature"),
        Err(reply) => return reply,
    };
    match pairings(state).get(id).filter(|p| p.key == key) {
        Some(pairing) => json(200, &PairStatus { taken: pairing.taken.clone() }),
        None => error(404, "that sign-in code has expired"),
    }
}

/// The Android app's package, for `asset_links`.
const ANDROID_PACKAGE: &str = "nl.p51.synththing";

/// `GET /pair`: where a sign-in link lands outside the app (a phone
/// without synththing, or one that doesn't open the link in it). The code
/// stays in the address's `#`, which never reaches the server; the page's
/// button hands it to the app.
pub(super) fn page() -> Reply {
    tiny_http::Response::from_string(PAGE).with_header(header("Content-Type", "text/html; charset=utf-8"))
}

const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in to synththing</title>
<style>
body { font-family: system-ui, sans-serif; max-width: 30em; margin: 3em auto; padding: 0 1em; line-height: 1.5; }
a.button { display: inline-block; padding: 0.6em 1.2em; border-radius: 0.4em; background: #6b5bd6; color: white; text-decoration: none; }
code { word-break: break-all; }
</style>
</head>
<body>
<h1>Sign in to synththing</h1>
<p>This is a sign-in code from another device of yours. It works once, for 10 minutes.</p>
<p><a class="button" id="open" href="#">Open in synththing</a></p>
<p>On a computer, choose <b>Sign in with a link...</b> under Preferences &gt; Online Library and paste this page's address.</p>
<p>No synththing yet? <a href="https://github.com/Dissssy/synththing/releases">Get it from GitHub</a>.</p>
<script>document.getElementById("open").href = "synththing:" + location.href;</script>
</body>
</html>
"##;

/// `GET /.well-known/assetlinks.json`: the Android app may open this
/// server's links (`android_fingerprints` in `server.json`).
pub(super) fn asset_links(state: &State) -> Reply {
    if state.config.android_fingerprints.is_empty() {
        return error(404, "no app opens this server's links");
    }
    json(
        200,
        &serde_json::json!([{
            "relation": ["delegate_permission/common.handle_all_urls"],
            "target": {
                "namespace": "android_app",
                "package_name": ANDROID_PACKAGE,
                "sha256_cert_fingerprints": state.config.android_fingerprints,
            },
        }]),
    )
}

