//! The user's identity on library servers (docs/SERVER.md): an Ed25519
//! key pair made by the app, nothing else. The public key is who you are
//! (shown as a short ID, `#k3f9q2xa`); the secret half signs uploads,
//! deletes and the like, and lives in the config folder
//! (`identity.key`). Exported, it's a small JSON file, sealed with a
//! passphrase if one is given (Argon2id stretches the passphrase into a
//! key, ChaCha20-Poly1305 seals the secret with it).

use std::path::{Path, PathBuf};

use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use super::{hex, key_id, sign_hex, unhex};

pub struct Identity {
    key: SigningKey,
}

/// An exported identity.
#[derive(Serialize, Deserialize)]
struct Exported {
    synththing_identity: u32,
    id: String,
    /// The secret, in hex, if it isn't sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    /// Or sealed with a passphrase: Argon2id salt, nonce and the sealed
    /// secret, in hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    salt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed: Option<String>,
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn passphrase_cipher(passphrase: &str, salt: &[u8]) -> Result<ChaCha20Poly1305, String> {
    let mut key = [0u8; 32];
    Argon2::default().hash_password_into(passphrase.as_bytes(), salt, &mut key).map_err(|e| e.to_string())?;
    ChaCha20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())
}

impl Identity {
    /// Where it's kept.
    pub fn path() -> Option<PathBuf> {
        crate::config::config_dir().ok().map(|d| d.join("identity.key"))
    }

    /// An identity from its secret key, in hex (as CI is given it).
    pub fn from_secret_hex(text: &str) -> Option<Self> {
        Self::from_hex(text)
    }

    fn from_hex(text: &str) -> Option<Self> {
        let bytes = <[u8; 32]>::try_from(unhex(text.trim())?).ok()?;
        Some(Self { key: SigningKey::from_bytes(&bytes) })
    }

    /// The saved identity, if there is one.
    pub fn load() -> Option<Self> {
        Self::from_hex(&std::fs::read_to_string(Self::path()?).ok()?)
    }

    /// A new one (not saved yet).
    pub fn generate() -> Result<Self, String> {
        Ok(Self { key: super::new_signing_key().map_err(|e| e.to_string())? })
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no config folder")?;
        save_secret(&path, &hex(&self.key.to_bytes()))
    }

    /// The public key, in hex.
    pub fn public(&self) -> String {
        hex(&self.key.verifying_key().to_bytes())
    }

    /// The short ID (`k3f9q2xa`).
    pub fn id(&self) -> String {
        key_id(&self.public())
    }

    pub fn sign(&self, message: &[u8]) -> String {
        sign_hex(&self.key, message)
    }

    /// The identity as a file's contents, sealed with `passphrase` unless
    /// it's empty.
    pub fn export(&self, passphrase: &str) -> Result<String, String> {
        let secret = self.key.to_bytes();
        let mut exported =
            Exported { synththing_identity: 1, id: self.id(), key: None, salt: None, nonce: None, sealed: None };
        if passphrase.is_empty() {
            exported.key = Some(hex(&secret));
        } else {
            let salt: [u8; 16] = random()?;
            let nonce: [u8; 12] = random()?;
            let sealed = passphrase_cipher(passphrase, &salt)?
                .encrypt(&Nonce::from(nonce), secret.as_slice())
                .map_err(|e| e.to_string())?;
            exported.salt = Some(hex(&salt));
            exported.nonce = Some(hex(&nonce));
            exported.sealed = Some(hex(&sealed));
        }
        serde_json::to_string_pretty(&exported).map_err(|e| e.to_string())
    }

    /// Whether an exported identity needs its passphrase.
    pub fn needs_passphrase(text: &str) -> bool {
        serde_json::from_str::<Exported>(text).is_ok_and(|e| e.sealed.is_some())
    }

    /// An exported identity, opened with `passphrase` if it's sealed.
    pub fn import(text: &str, passphrase: &str) -> Result<Self, String> {
        let exported: Exported = serde_json::from_str(text).map_err(|_| "that isn't a synththing identity file")?;
        let identity = match (exported.key, exported.salt, exported.nonce, exported.sealed) {
            (Some(key), ..) => Self::from_hex(&key).ok_or("the file's key is damaged")?,
            (None, Some(salt), Some(nonce), Some(sealed)) => {
                let (salt, sealed) = (unhex(&salt).ok_or("damaged file")?, unhex(&sealed).ok_or("damaged file")?);
                let nonce = <[u8; 12]>::try_from(unhex(&nonce).ok_or("damaged file")?).map_err(|_| "damaged file")?;
                let secret = passphrase_cipher(passphrase, &salt)?
                    .decrypt(&Nonce::from(nonce), sealed.as_slice())
                    .map_err(|_| "wrong passphrase")?;
                Self::from_hex(&hex(&secret)).ok_or("the file's key is damaged")?
            }
            _ => return Err("the file has no key in it".into()),
        };
        if identity.id() != exported.id {
            return Err("the file's key doesn't match its ID (damaged?)".into());
        }
        Ok(identity)
    }
}

/// Write a secret where only its owner can read it (on Linux and macOS).
pub fn save_secret(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_and_import() {
        let identity = Identity::generate().unwrap();
        let plain = identity.export("").unwrap();
        assert!(!Identity::needs_passphrase(&plain));
        assert_eq!(Identity::import(&plain, "").unwrap().public(), identity.public());

        let sealed = identity.export("correct horse").unwrap();
        assert!(Identity::needs_passphrase(&sealed));
        assert!(!sealed.contains(&hex(&identity.key.to_bytes())), "the secret isn't in it");
        assert_eq!(Identity::import(&sealed, "correct horse").unwrap().id(), identity.id());
        assert_eq!(Identity::import(&sealed, "wrong").err().as_deref(), Some("wrong passphrase"));
        assert!(Identity::import("{}", "").is_err());
    }
}
