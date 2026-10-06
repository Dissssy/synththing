//! Identity changes (docs/SERVER.md, Authorities): a rotation record says
//! "key `old` is now key `new`", countersigned by an authority, a server
//! whose key is trusted for this. A key-signed rotation (`how: "key"`) is
//! asked for by the old key and the new one both; a recovery
//! (`how: "email"`) is confirmed by a code sent to the email attached to
//! the identity. Records chain: a key rotated twice has two, the second's
//! `old` the first's `new`. Servers check them offline, against the
//! authorities they trust, and move what the old key had to the new one.

use serde::{Deserialize, Serialize};

use super::verify_hex;

/// The official server's key: the authority servers and the app trust
/// unless they're told otherwise.
pub const OFFICIAL_AUTHORITY: &str = "d5dd8cc8c97ee8669b5d92ec3822202d4323343ca3d19a15b49262d2c34a9450";

/// A rotation, as the authority issued it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rotation {
    /// Keys, in hex.
    pub old: String,
    pub new: String,
    /// Unix seconds.
    pub time: i64,
    /// `"key"` (asked for with the old key) or `"email"` (a recovery).
    pub how: String,
    /// The authority's key, and its signature over `rotation_message`.
    pub authority: String,
    pub signature: String,
}

/// What an authority signs for a rotation.
pub fn rotation_message(authority: &str, old: &str, new: &str, time: i64, how: &str) -> Vec<u8> {
    format!("synththing/rotation/v1\n{authority}\n{old}\n{new}\n{time}\n{how}").into_bytes()
}

/// What the new key signs to show it's held by whoever asks for a rotation
/// (or a recovery) to it, for `authority`, at `time`.
pub fn new_key_message(authority: &str, old: &str, new: &str, time: i64) -> Vec<u8> {
    format!("synththing/new-key/v1\n{authority}\n{old}\n{new}\n{time}").into_bytes()
}

impl Rotation {
    /// Signed by an authority in `trusted` (keys, hex).
    pub fn verify(&self, trusted: &[String]) -> bool {
        trusted.contains(&self.authority)
            && verify_hex(
                &self.authority,
                &rotation_message(&self.authority, &self.old, &self.new, self.time, &self.how),
                &self.signature,
            )
    }
}

/// Every record trusted, each one's `old` the one before's `new`.
pub fn verify_chain(chain: &[Rotation], trusted: &[String]) -> Result<(), String> {
    for (i, record) in chain.iter().enumerate() {
        if !record.verify(trusted) {
            return Err(format!("rotation {} isn't signed by an authority this server trusts", i + 1));
        }
        if i > 0 && chain[i - 1].new != record.old {
            return Err(format!("rotation {} doesn't follow on from the one before", i + 1));
        }
    }
    Ok(())
}

/// `POST /api/v1/identity/rotate`, signed by the old key: the new key and
/// its signature over `new_key_message`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RotateRequest {
    pub new: String,
    pub time: i64,
    pub new_signature: String,
}

/// `POST /api/v1/identity/email`, signed: attach `address`, or change to
/// it, or (without one) take the email off. Changing or taking it off
/// needs the `current` address, which gets a code too.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct EmailRequest {
    pub address: Option<String>,
    pub current: Option<String>,
}

/// `POST /api/v1/identity/email/confirm`, signed: the codes the addresses
/// got (`code` the new one's, `current_code` the current one's).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct EmailConfirm {
    pub code: Option<String>,
    pub current_code: Option<String>,
}

/// `POST /api/v1/identity/status`, signed: what the authority knows of
/// the key.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct IdentityStatus {
    /// An email is attached, for recovery.
    pub email: bool,
    /// What it'd mean to change it now: the authority's mail is on.
    pub mail: bool,
}

/// `POST /api/v1/identity/recover`, unsigned: the email attached to an
/// identity, and the key to recover it to (with its signature over
/// `new_key_message`, `old` empty).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecoverRequest {
    pub address: String,
    pub new: String,
    pub time: i64,
    pub new_signature: String,
}

/// `POST /api/v1/identity/recover/confirm`, unsigned: the code the email
/// got, for the key recovering to. Answered with the `Rotation`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecoverConfirm {
    pub new: String,
    pub code: String,
}

/// An email address as it's compared: trimmed, lowercase.
pub fn normalize_email(address: &str) -> String {
    address.trim().to_lowercase()
}

/// Whether `address` looks like an email address (something@something.tld).
pub fn is_email(address: &str) -> bool {
    let address = address.trim();
    match address.split_once('@') {
        Some((user, domain)) => {
            !user.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && address.len() <= 254
                && !address.chars().any(|c| c.is_whitespace() || c == '<' || c == '>' || c == ',')
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{hex, new_signing_key, sign_hex};

    #[test]
    fn records_and_chains() {
        let authority = new_signing_key().unwrap();
        let authority_hex = hex(&authority.verifying_key().to_bytes());
        let other = hex(&new_signing_key().unwrap().verifying_key().to_bytes());
        let sign = |old: &str, new: &str| {
            let signature = sign_hex(&authority, &rotation_message(&authority_hex, old, new, 5, "key"));
            Rotation { old: old.into(), new: new.into(), time: 5, how: "key".into(), authority: authority_hex.clone(), signature }
        };
        let (a, b, c) = ("aa".to_string(), "bb".to_string(), "cc".to_string());
        let trusted = vec![authority_hex.clone()];
        assert!(verify_chain(&[sign(&a, &b), sign(&b, &c)], &trusted).is_ok());
        assert!(verify_chain(&[sign(&a, &b), sign(&a, &c)], &trusted).unwrap_err().contains("follow"));
        assert!(verify_chain(&[sign(&a, &b)], &[other]).unwrap_err().contains("trusts"));
        let mut forged = sign(&a, &b);
        forged.new = c;
        assert!(!forged.verify(&trusted));
        assert!(is_email(" Someone@Example.org ") && !is_email("nope") && !is_email("a@b") && !is_email("a b@c.d"));
        assert_eq!(normalize_email(" Someone@Example.org "), "someone@example.org");
    }
}
