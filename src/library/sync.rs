//! Sync (docs/SERVER.md, Sync): what's synced between the devices signed
//! in as one identity is kept on the authority as named blobs
//! (`settings`, ...), each sealed with the identity's sync key
//! (`identity::sync_key`) before it leaves the device, so the server only
//! ever holds ciphertext. Each blob has a revision: a write says which
//! revision it was made from, and one made from an older revision than
//! the server's is refused, for the device to merge and write again.
//! Every write is announced on the notification stream, so the other
//! devices fetch it.
//!
//! Songs go between the devices through the relay: one device seals a
//! song's file and leaves it on the authority (`POST /api/v1/relay/{id}`)
//! for another to take (`.../get`), by an ID the server can't tell the
//! song from (`relay_id`). The synced data says which songs are wanted and
//! which are waiting there (`song_sync.rs`).

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};

/// The most a sealed blob can be (base64).
pub const MAX_DATA: usize = 1024 * 1024;
/// The most blobs one identity can have.
pub const MAX_BLOBS: usize = 16;

/// Whether `name` is a blob's name: 1 to 32 of `a-z`, `0-9`, `-` and `_`.
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// `POST /api/v1/sync/{name}` (signed): write a blob, made from revision
/// `base` (0 for a blob the device has never seen).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Write {
    pub base: i64,
    pub data: String,
}

/// What a write did: `stored` it as `revision`, or refused it because the
/// server has `revision`, newer than the write's base.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Written {
    pub stored: bool,
    pub revision: i64,
}

/// `POST /api/v1/sync/{name}/get` (signed): the blob, if there is one
/// (revision 0 and no data if not).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Blob {
    pub revision: i64,
    #[serde(default)]
    pub data: Option<String>,
}

/// On the notification stream: a blob was written.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Changed {
    pub name: String,
    pub revision: i64,
}

/// A stream event carrying a `Changed` (`{"sync": {...}}`), which older
/// apps take for a notification they can't read, and skip.
#[derive(Serialize, Deserialize)]
pub struct ChangedEvent {
    pub sync: Changed,
}

/// `plain` sealed with the sync key: the nonce, then the sealed bytes.
pub fn seal_bytes(sync_key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let sealed = ChaCha20Poly1305::new(sync_key.into())
        .encrypt(&Nonce::from(nonce), plain)
        .map_err(|e| e.to_string())?;
    Ok([nonce.as_slice(), &sealed].concat())
}

/// What `seal_bytes` sealed, opened with the same sync key.
pub fn open_bytes(sync_key: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>, String> {
    let damaged = "the synced data is damaged";
    if sealed.len() <= 12 {
        return Err(damaged.into());
    }
    let (nonce, sealed) = sealed.split_at(12);
    let nonce = <[u8; 12]>::try_from(nonce).map_err(|_| damaged)?;
    ChaCha20Poly1305::new(sync_key.into())
        .decrypt(&Nonce::from(nonce), sealed)
        .map_err(|_| "the synced data doesn't open with this device's sync key".to_string())
}

/// `plain` sealed with the sync key (`seal_bytes`), in base64.
pub fn seal(sync_key: &[u8; 32], plain: &[u8]) -> Result<String, String> {
    Ok(base64::engine::general_purpose::STANDARD.encode(seal_bytes(sync_key, plain)?))
}

/// What `seal` sealed, opened with the same sync key. An error for one
/// sealed with another key: the synced data from before a recovery, or a
/// damaged blob.
pub fn open(sync_key: &[u8; 32], sealed: &str) -> Result<Vec<u8>, String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(sealed.trim()).map_err(|_| "the synced data is damaged")?;
    open_bytes(sync_key, &bytes)
}

/// Whether `id` is a relayed file's ID: 32 of `0-9a-f`.
pub fn valid_relay_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The ID a song goes by on the relay (`POST /api/v1/relay/{id}`): its key
/// hashed with the sync key, so the server can't tell which song it is.
pub fn relay_id(sync_key: &[u8; 32], song: &str) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::new().chain_update(sync_key).chain_update(song.as_bytes()).finalize();
    super::hex(&hash[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("settings"));
        assert!(valid_name("play_lists-2"));
        assert!(!valid_name(""));
        assert!(!valid_name("Settings"));
        assert!(!valid_name("../x"));
        assert!(!valid_name(&"a".repeat(33)));
    }

    #[test]
    fn sealing() {
        let sealed = seal(&[1; 32], b"{\"theme\":\"dark\"}").unwrap();
        assert_eq!(open(&[1; 32], &sealed).unwrap(), b"{\"theme\":\"dark\"}");
        assert!(open(&[2; 32], &sealed).is_err());
        assert!(open(&[1; 32], "AAAA").is_err());
        let song = seal_bytes(&[1; 32], b"MThd").unwrap();
        assert_eq!(open_bytes(&[1; 32], &song).unwrap(), b"MThd");
    }

    #[test]
    fn relay_ids() {
        let id = relay_id(&[1; 32], "ab.mid");
        assert!(valid_relay_id(&id));
        assert_ne!(id, relay_id(&[2; 32], "ab.mid"), "another identity's");
        assert!(!valid_relay_id("../../etc/passwd000000000000000000"));
    }
}
