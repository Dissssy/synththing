//! The hubs this server hosts (docs/SERVER.md, Hosted hubs).
//!
//! The hubs themselves are made, sized and deleted by a hub host: a
//! separate service on the same machine (`hubhost serve`), which holds
//! what that takes and listens only to this server. An admin's
//! `HubAction` is passed on to it at `server.json`'s `hubhost`, signed
//! with the server's own key over [`message`], with a time and a nonce so
//! a request can't be sent twice.
//!
//! A hub is made for someone: its owner (an identity) gets a `hub_granted`
//! notification with the hub's address and the claim code it was made
//! with, and their app signs the hub in with those, talking to the hub
//! itself. A sign-in link opens the identity's keys, so none ever comes
//! through here.

use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::{json, Value};

use super::{moderation, notifications, now, State};
use crate::library::{hex, key_id, sha256_hex, sign_hex, HubAction};

const TIME_HEADER: &str = "x-hubhost-time";
const NONCE_HEADER: &str = "x-hubhost-nonce";
const SIGNATURE_HEADER: &str = "x-hubhost-signature";

type Acted = Result<Value, (u16, String)>;

/// What the server signs for the hub host.
fn message(method: &str, path: &str, time: i64, nonce: &str, body: &[u8]) -> Vec<u8> {
    format!("hubhost\n{method}\n{path}\n{time}\n{nonce}\n{}", sha256_hex(body)).into_bytes()
}

pub(super) fn act(state: &State, action: &HubAction) -> Acted {
    let base = state.config.hubhost.as_deref().ok_or((404, "this server doesn't host hubs".to_string()))?;
    let base = base.trim_end_matches('/');
    match action {
        HubAction::Add { name, size_gb, owner, note } => {
            // (Before anything's made: a hub nobody can be told about is no use.)
            let owner = moderation::resolve_key(&state.db(), owner)?;
            let made = add(base, &state.key, name, *size_gb, &key_id(&owner), note)?;
            let (Some(url), Some(code)) = (made["url"].as_str(), made["claim"].as_str()) else {
                return Err((502, "the hub host made the hub, but didn't say where it is or how it's claimed".into()));
            };
            let granted = json!({ "name": name, "url": url, "code": code, "size_gb": size_gb });
            notifications::notify(state, &state.db(), &owner, "hub_granted", granted, true);
            Ok(json!({ "done": format!("Made {url}: #{} is being told.", key_id(&owner)) }))
        }
        HubAction::Destroy { name } => {
            let done = act_at(base, &state.key, action)?;
            // (Nobody's asked to sign in a hub that's gone.)
            let _ = state.db().execute(
                "UPDATE notifications SET handled = 1, read = 1, resolution = 'withdrawn'
                 WHERE kind = 'hub_granted' AND handled = 0 AND json_extract(data, '$.name') = ?",
                [name],
            );
            Ok(done)
        }
        HubAction::List | HubAction::Resize { .. } => act_at(base, &state.key, action),
    }
}

/// Have the hub host make a hub: what it says of it (`url`, `claim`).
fn add(base: &str, key: &SigningKey, name: &str, size_gb: u64, owner: &str, note: &str) -> Acted {
    let body = json!({ "name": named(name)?, "size_gb": size_gb, "owner": owner, "note": filled(note) });
    send(base, key, "POST", "/v1/hubs", Some(body))
}

fn act_at(base: &str, key: &SigningKey, action: &HubAction) -> Acted {
    let hub = |name: &str| named(name).map(|name| format!("/v1/hubs/{name}"));
    match action {
        HubAction::List => {
            let mut hubs = send(base, key, "GET", "/v1/hubs", None)?;
            let pool = send(base, key, "GET", "/v1/pool", None)?;
            for field in ["pool", "monthly_cost", "monthly_cost_error"] {
                hubs[field] = pool[field].clone();
            }
            Ok(hubs)
        }
        HubAction::Add { name, size_gb, owner, note } => add(base, key, name, *size_gb, owner, note),
        HubAction::Resize { name, size_gb } => {
            send(base, key, "PATCH", &hub(name)?, Some(json!({ "size_gb": size_gb })))?;
            Ok(json!({ "done": format!("{name} has {size_gb} GB now.") }))
        }
        HubAction::Destroy { name } => {
            send(base, key, "DELETE", &hub(name)?, Some(json!({ "confirm": name })))?;
            Ok(json!({ "done": format!("Deleted {name} and its songs.") }))
        }
    }
}

/// A hub's name, which goes into the address asked for and the hub's own.
fn named(name: &str) -> Result<&str, (u16, String)> {
    let ok = (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if ok {
        Ok(name)
    } else {
        Err((400, "a hub's name is 1 to 32 lowercase letters, digits and dashes, not starting or ending with a dash".into()))
    }
}

fn filled(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|t| !t.is_empty())
}

/// One signed request. The hub host's own refusals come back in its words.
fn send(base: &str, key: &SigningKey, method: &str, path: &str, body: Option<Value>) -> Acted {
    let body = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|e| (500, format!("couldn't make a nonce: {e}")))?;
    let (time, nonce) = (now(), hex(&nonce));
    let request = ureq::http::Request::builder()
        .method(method)
        .uri(format!("{base}{path}"))
        .header("content-type", "application/json")
        .header(TIME_HEADER, time.to_string())
        .header(NONCE_HEADER, &nonce)
        .header(SIGNATURE_HEADER, sign_hex(key, &message(method, path, time, &nonce, &body)))
        .body(body)
        .map_err(|e| (500, e.to_string()))?;
    // (Making a hub pulls its image and may grow the pool.)
    let agent: ureq::Agent =
        ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(300))).build().into();
    let mut response = agent.run(request).map_err(|e| (502, format!("couldn't reach the hub host: {e}")))?;
    let status = response.status().as_u16();
    let reply: Value = response.body_mut().read_json().unwrap_or_default();
    if status < 300 {
        return Ok(reply);
    }
    let said = reply["error"].as_str().map_or_else(|| format!("it answered {status}"), str::to_string);
    println!("the hub host refused {method} {path} ({status}): {said}");
    Err((if status == 422 { 400 } else { 502 }, format!("the hub host says: {said}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::verify_hex;

    /// A request's method, path and body.
    type Asked = (String, String, Value);

    /// A hub host that answers `replies` in turn, and says what it was asked
    /// (method, path, body) once each was signed by `key`.
    fn host(key: &SigningKey, replies: Vec<(u16, Value)>) -> (String, std::thread::JoinHandle<Vec<Asked>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let key = hex(key.verifying_key().as_bytes());
        let thread = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for (status, reply) in replies {
                let mut request = server.recv().unwrap();
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).unwrap();
                let headers: Vec<(String, String)> =
                    request.headers().iter().map(|h| (h.field.to_string().to_lowercase(), h.value.to_string())).collect();
                let header = |name: &str| headers.iter().find(|(field, _)| field == name).map(|(_, value)| value.clone()).unwrap_or_default();
                let (method, path) = (request.method().to_string(), request.url().to_string());
                let time: i64 = header(TIME_HEADER).parse().unwrap();
                assert!((now() - time).abs() < 60);
                let signed = message(&method, &path, time, &header(NONCE_HEADER), &body);
                assert!(verify_hex(&key, &signed, &header(SIGNATURE_HEADER)), "{method} {path} isn't signed by the server");
                asked.push((method, path, serde_json::from_slice(&body).unwrap_or_default()));
                request.respond(tiny_http::Response::from_string(reply.to_string()).with_status_code(status)).unwrap();
            }
            asked
        });
        (base, thread)
    }

    #[test]
    fn actions_reach_the_hub_host_signed() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let (base, host) = host(
            &key,
            vec![
                (200, json!({ "name": "sophie", "url": "https://sophie.hubs.example" })),
                (200, json!({})),
                (200, json!({})),
                (200, json!({ "hubs": [{ "name": "sophie", "size_gb": 100, "used_bytes": 5, "state": "running" }] })),
                (200, json!({ "pool": { "granted_gb": 100 }, "monthly_cost": { "net": 4.4, "currency": "EUR" } })),
            ],
        );
        let add = HubAction::Add { name: "sophie".into(), size_gb: 50, owner: "axk3xbkl".into(), note: " ".into() };
        assert_eq!(act_at(&base, &key, &add).unwrap()["url"], "https://sophie.hubs.example");
        act_at(&base, &key, &HubAction::Resize { name: "sophie".into(), size_gb: 100 }).unwrap();
        act_at(&base, &key, &HubAction::Destroy { name: "sophie".into() }).unwrap();
        let listed: crate::library::HostedHubs = serde_json::from_value(act_at(&base, &key, &HubAction::List).unwrap()).unwrap();
        assert_eq!(listed.hubs[0].used_bytes, Some(5));
        assert_eq!(listed.pool.granted_gb, 100);
        assert_eq!(listed.monthly_cost.unwrap().currency, "EUR");

        let asked = host.join().unwrap();
        let asked: Vec<_> = asked.iter().map(|(m, p, b)| (m.as_str(), p.as_str(), b.clone())).collect();
        assert_eq!(
            asked,
            [
                ("POST", "/v1/hubs", json!({ "name": "sophie", "size_gb": 50, "owner": "axk3xbkl", "note": null })),
                ("PATCH", "/v1/hubs/sophie", json!({ "size_gb": 100 })),
                ("DELETE", "/v1/hubs/sophie", json!({ "confirm": "sophie" })),
                ("GET", "/v1/hubs", Value::Null),
                ("GET", "/v1/pool", Value::Null),
            ]
        );
    }

    #[test]
    fn refusals_come_back_in_the_hub_hosts_words() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let (base, host) = host(&key, vec![(422, json!({ "error": "there's a hub called sophie already" }))]);
        let add = HubAction::Add { name: "sophie".into(), size_gb: 50, owner: "axk3xbkl".into(), note: String::new() };
        assert_eq!(act_at(&base, &key, &add), Err((400, "the hub host says: there's a hub called sophie already".into())));
        host.join().unwrap();
        // A name that would change the address asked for never leaves.
        let bad = HubAction::Destroy { name: "a/../b".into() };
        assert_eq!(act_at("http://127.0.0.1:1", &key, &bad).unwrap_err().0, 400);
    }
}
