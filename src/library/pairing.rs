//! Signing in another device (docs/SERVER.md, Pairing): a signed-in app
//! seals its exported identity with a random secret and parks it on a
//! server for a few minutes; the link it shows (as a QR code, or to copy)
//! carries the server, the parked handoff's ID and the secret. The other
//! device opens the link, takes the handoff (once: it's gone from the
//! server after that) and opens it with the secret. The secret is after
//! the link's `#`, which browsers never send, so the server only ever
//! holds the sealed copy.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};

use super::{hex, unhex};

/// How long a parked handoff waits to be taken.
pub const PAIR_SECONDS: i64 = 10 * 60;
/// The most a sealed handoff can be (hex).
pub const MAX_SEALED: usize = 64 * 1024;
/// The most a device's name can be.
pub const MAX_DEVICE_NAME: usize = 64;

/// `POST /api/v1/pair` (signed): park a sealed handoff.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Park {
    pub sealed: String,
}

/// What parking gave back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Parked {
    pub id: String,
    /// Unix seconds.
    pub expires: i64,
}

/// `POST /api/v1/pair/{id}/take`: the device taking it, by name.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Take {
    pub device: String,
}

/// `POST /api/v1/pair/{id}` (signed by the key that parked it): whether
/// it's been taken, and by which device.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PairStatus {
    #[serde(default)]
    pub taken: Option<String>,
}

/// A link to a parked handoff: `<server>/pair#<id>.<secret>`.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub server: String,
    pub id: String,
    pub secret: [u8; 32],
}

impl Link {
    /// A new secret, for a handoff to park on `server`.
    pub fn secret() -> Result<[u8; 32], String> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| e.to_string())?;
        Ok(secret)
    }

    pub fn url(&self) -> String {
        format!("{}/pair#{}.{}", self.server, self.id, hex(&self.secret))
    }

    /// A link as pasted (spaces around it are fine).
    pub fn parse(text: &str) -> Option<Self> {
        let (server, rest) = text.trim().split_once("/pair#")?;
        let (id, secret) = rest.split_once('.')?;
        let secret = <[u8; 32]>::try_from(unhex(secret)?).ok()?;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) || !server.starts_with("http") {
            return None;
        }
        Some(Self { server: super::client::normalize_url(server), id: id.to_string(), secret })
    }
}

/// `plain` sealed with `secret`: the nonce and the sealed bytes, in hex.
pub fn seal(secret: &[u8; 32], plain: &[u8]) -> Result<String, String> {
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let sealed = ChaCha20Poly1305::new(secret.into())
        .encrypt(&Nonce::from(nonce), plain)
        .map_err(|e| e.to_string())?;
    Ok(hex(&[nonce.as_slice(), &sealed].concat()))
}

/// What `seal` sealed, opened with the same secret.
pub fn open(secret: &[u8; 32], sealed: &str) -> Result<Vec<u8>, String> {
    let bytes = unhex(sealed).filter(|b| b.len() > 12).ok_or("the handoff is damaged")?;
    let (nonce, sealed) = bytes.split_at(12);
    let nonce = <[u8; 12]>::try_from(nonce).map_err(|_| "the handoff is damaged")?;
    ChaCha20Poly1305::new(secret.into())
        .decrypt(&Nonce::from(nonce), sealed)
        .map_err(|_| "the handoff doesn't open with this link's secret".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_round_trip() {
        let link = Link { server: "https://example.org".into(), id: "ab12".into(), secret: [7; 32] };
        assert_eq!(Link::parse(&format!("  {}\n", link.url())), Some(link));
        assert_eq!(Link::parse("https://example.org/pair#ab12"), None);
        assert_eq!(Link::parse("https://example.org/pair#zz.0707"), None);
        assert_eq!(Link::parse("#k3f9q2xa"), None);
    }

    #[test]
    fn sealing() {
        let secret = Link::secret().unwrap();
        let sealed = seal(&secret, b"the identity").unwrap();
        assert_eq!(open(&secret, &sealed).unwrap(), b"the identity");
        assert!(open(&[0; 32], &sealed).is_err());
        assert!(open(&secret, "00").is_err());
    }
}
