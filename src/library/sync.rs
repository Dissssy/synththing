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
//! Songs go between the devices through the relay, sealed in chunks
//! (`SealedReader`, `open_stream`) so no device holds a whole one: one
//! device streams a song straight to another listening for it (`POST
//! /api/v1/relay/{id}/send/{inbox}`), or leaves it on the authority (`POST
//! /api/v1/relay/{id}`) for another to take (`.../get`), by an ID the
//! server can't tell the song from (`relay_id`). The synced data says
//! which songs are wanted and which are waiting there (`song_sync.rs`).

use std::io::{self, Read};

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

/// A song is sealed in chunks of this much: each on its own, with the
/// nonce's last byte marking the last (shorter, maybe empty) one, so a
/// stream cut short doesn't open.
const CHUNK: usize = 64 * 1024;
/// What sealing adds to a chunk.
const TAG: usize = 16;
/// The random part of each chunk's nonce, first in the stream (the rest is
/// the chunk's number and whether it's the last).
const PREFIX: usize = 7;

fn chunk_nonce(prefix: &[u8; PREFIX], n: u32, last: bool) -> Nonce {
    let mut nonce = [0u8; 12];
    nonce[..PREFIX].copy_from_slice(prefix);
    nonce[PREFIX..11].copy_from_slice(&n.to_be_bytes());
    nonce[11] = last.into();
    Nonce::from(nonce)
}

/// Read until `buf` is full or `from` ends: how much was read.
fn fill(from: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match from.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// How long `plain_len` bytes are, sealed by `SealedReader`.
pub fn sealed_len(plain_len: u64) -> u64 {
    PREFIX as u64 + plain_len + (plain_len / CHUNK as u64 + 1) * TAG as u64
}

/// A song (`plain`) sealed with the sync key as it's read.
pub struct SealedReader<R> {
    plain: R,
    cipher: ChaCha20Poly1305,
    prefix: [u8; PREFIX],
    n: u32,
    sealed: Vec<u8>,
    at: usize,
    done: bool,
}

impl<R: Read> SealedReader<R> {
    pub fn new(sync_key: &[u8; 32], plain: R) -> Result<Self, String> {
        let mut prefix = [0u8; PREFIX];
        getrandom::fill(&mut prefix).map_err(|e| e.to_string())?;
        let cipher = ChaCha20Poly1305::new(sync_key.into());
        Ok(Self { plain, cipher, prefix, n: 0, sealed: prefix.to_vec(), at: 0, done: false })
    }
}

impl<R: Read> Read for SealedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.at == self.sealed.len() {
            if self.done {
                return Ok(0);
            }
            let mut chunk = vec![0; CHUNK];
            let len = fill(&mut self.plain, &mut chunk)?;
            self.done = len < CHUNK;
            self.sealed = self
                .cipher
                .encrypt(&chunk_nonce(&self.prefix, self.n, self.done), &chunk[..len])
                .map_err(|_| io::Error::other("couldn't seal the song"))?;
            self.n += 1;
            self.at = 0;
        }
        let n = buf.len().min(self.sealed.len() - self.at);
        buf[..n].copy_from_slice(&self.sealed[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Open a song `SealedReader` sealed, writing it to `to` as it comes. One
/// that doesn't open with this sync key, or was cut short, is
/// `InvalidData`.
pub fn open_stream(sync_key: &[u8; 32], mut sealed: impl Read, mut to: impl io::Write) -> io::Result<()> {
    let damaged = || io::Error::new(io::ErrorKind::InvalidData, "the song doesn't open with this device's sync key");
    let mut prefix = [0u8; PREFIX];
    if fill(&mut sealed, &mut prefix)? < PREFIX {
        return Err(damaged());
    }
    let cipher = ChaCha20Poly1305::new(sync_key.into());
    let mut chunk = vec![0; CHUNK + TAG];
    for n in 0.. {
        let len = fill(&mut sealed, &mut chunk)?;
        let last = len < chunk.len();
        let plain = cipher.decrypt(&chunk_nonce(&prefix, n, last), &chunk[..len]).map_err(|_| damaged())?;
        to.write_all(&plain)?;
        if last {
            break;
        }
    }
    Ok(())
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

/// The ID a device listens for songs streamed to it by (`POST
/// /api/v1/relay/inbox/{inbox}`): its ID hashed with the sync key, so the
/// ID itself never leaves the device.
pub fn inbox_id(sync_key: &[u8; 32], device: &str) -> String {
    relay_id(sync_key, &format!("inbox/{device}"))
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
        // Songs, in chunks: any length, cut short or with another key, no.
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 5] {
            let plain: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let mut sealed = Vec::new();
            SealedReader::new(&[1; 32], &plain[..]).unwrap().read_to_end(&mut sealed).unwrap();
            assert_eq!(sealed.len() as u64, sealed_len(len as u64));
            let mut opened = Vec::new();
            open_stream(&[1; 32], &sealed[..], &mut opened).unwrap();
            assert_eq!(opened, plain);
            let invalid = |sealed: &[u8], key| open_stream(key, sealed, io::sink()).unwrap_err().kind() == io::ErrorKind::InvalidData;
            assert!(invalid(&sealed, &[2; 32]));
            assert!(invalid(&sealed[..sealed.len() - 1], &[1; 32]));
            if len >= CHUNK {
                assert!(invalid(&sealed[..PREFIX + CHUNK + TAG], &[1; 32]), "cut at a chunk");
            }
        }
    }

    #[test]
    fn relay_ids() {
        let id = relay_id(&[1; 32], "ab.mid");
        assert!(valid_relay_id(&id));
        assert_ne!(id, relay_id(&[2; 32], "ab.mid"), "another identity's");
        assert!(!valid_relay_id("../../etc/passwd000000000000000000"));
    }
}
