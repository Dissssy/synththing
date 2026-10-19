//! Signing in another device (`library/pairing.rs`): sealed handoffs,
//! parked by a signed-in app and taken once by the device it's signing
//! in. They're kept in memory only, for `PAIR_SECONDS`: a restart drops
//! them, and the app just shows a new code.

use std::collections::HashMap;

use tiny_http::Request;

use super::moderation::refuse_banned;
use super::{error, json, now, read_body_or, signer, Reply, State};
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
