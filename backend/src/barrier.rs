//! The encryption barrier.
//!
//! Key hierarchy (same shape as HashiCorp Vault):
//!
//!   unseal shares ──Shamir──▶ root key (KEK, 32B, memory only)
//!                                  │ encrypts
//!                                  ▼
//!                          keyring  (stored at `core/keyring`)
//!                                  │ holds one data key per *term*
//!                                  ▼
//!                  every other value in storage (AES-256-GCM)
//!
//! Blob wire format: `[version:1][term:4 BE][nonce:12][ciphertext+tag]`.
//! The storage path is bound in as AEAD associated data, so a ciphertext
//! copied to another key in Redis fails to decrypt instead of being served.

use std::collections::BTreeMap;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{AppError, AppResult};

const BLOB_VERSION: u8 = 1;
const HEADER_LEN: usize = 1 + 4 + 12;
/// Term used for blobs encrypted directly with the root key (the keyring).
pub const ROOT_TERM: u32 = 0;

pub type Key = Zeroizing<[u8; 32]>;

pub fn random_key() -> Key {
    let mut k = Zeroizing::new([0u8; 32]);
    rand::thread_rng().fill_bytes(k.as_mut());
    k
}

pub fn encrypt_with(key: &[u8; 32], term: u32, aad: &str, plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad: aad.as_bytes() })
        .expect("AES-GCM encryption cannot fail for in-memory buffers");
    let mut out = Vec::with_capacity(HEADER_LEN + ct.len());
    out.push(BLOB_VERSION);
    out.extend_from_slice(&term.to_be_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Read the key term from a blob header without decrypting it.
pub fn blob_term(blob: &[u8]) -> AppResult<u32> {
    if blob.len() < HEADER_LEN || blob[0] != BLOB_VERSION {
        return Err(AppError::Internal("malformed barrier blob".into()));
    }
    Ok(u32::from_be_bytes(blob[1..5].try_into().unwrap()))
}

pub fn decrypt_with(key: &[u8; 32], aad: &str, blob: &[u8]) -> AppResult<Vec<u8>> {
    blob_term(blob)?;
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(
            Nonce::from_slice(&blob[5..HEADER_LEN]),
            Payload { msg: &blob[HEADER_LEN..], aad: aad.as_bytes() },
        )
        .map_err(|_| AppError::Internal(format!("barrier decrypt failed for `{aad}`")))
}

/// Serialized form of the keyring, as stored (encrypted) at `core/keyring`.
#[derive(Serialize, Deserialize)]
pub struct KeyringDoc {
    pub active_term: u32,
    pub keys: Vec<TermKey>,
}

#[derive(Serialize, Deserialize)]
pub struct TermKey {
    pub term: u32,
    /// base64 AES-256 key.
    pub key: String,
    pub installed_at: DateTime<Utc>,
}

/// The live, decrypted keyring held in memory while unsealed.
pub struct Barrier {
    keys: BTreeMap<u32, Key>,
    installed: BTreeMap<u32, DateTime<Utc>>,
    active_term: u32,
}

impl Barrier {
    /// A brand-new keyring with a single term-1 data key.
    pub fn fresh() -> Self {
        let mut b = Barrier { keys: BTreeMap::new(), installed: BTreeMap::new(), active_term: 0 };
        b.rotate();
        b
    }

    pub fn from_doc(doc: KeyringDoc) -> AppResult<Self> {
        let mut keys = BTreeMap::new();
        let mut installed = BTreeMap::new();
        for tk in doc.keys {
            let raw = Zeroizing::new(
                B64.decode(&tk.key).map_err(|_| AppError::Internal("corrupt keyring".into()))?,
            );
            let arr: [u8; 32] = raw
                .as_slice()
                .try_into()
                .map_err(|_| AppError::Internal("corrupt keyring key length".into()))?;
            keys.insert(tk.term, Zeroizing::new(arr));
            installed.insert(tk.term, tk.installed_at);
        }
        if !keys.contains_key(&doc.active_term) {
            return Err(AppError::Internal("keyring has no active term key".into()));
        }
        Ok(Barrier { keys, installed, active_term: doc.active_term })
    }

    pub fn to_doc(&self) -> KeyringDoc {
        KeyringDoc {
            active_term: self.active_term,
            keys: self
                .keys
                .iter()
                .map(|(term, k)| TermKey {
                    term: *term,
                    key: B64.encode(k.as_slice()),
                    installed_at: self.installed[term],
                })
                .collect(),
        }
    }

    /// Install a new data key; new writes use it, old terms stay readable.
    pub fn rotate(&mut self) -> u32 {
        let term = self.keys.keys().next_back().copied().unwrap_or(ROOT_TERM) + 1;
        self.keys.insert(term, random_key());
        self.installed.insert(term, Utc::now());
        self.active_term = term;
        term
    }

    pub fn active_term(&self) -> u32 {
        self.active_term
    }

    pub fn installed_at(&self) -> DateTime<Utc> {
        self.installed[&self.active_term]
    }

    pub fn has_term(&self, term: u32) -> bool {
        self.keys.contains_key(&term)
    }

    pub fn term_count(&self) -> usize {
        self.keys.len()
    }

    pub fn encrypt(&self, path: &str, plaintext: &[u8]) -> Vec<u8> {
        encrypt_with(&self.keys[&self.active_term], self.active_term, path, plaintext)
    }

    pub fn decrypt(&self, path: &str, blob: &[u8]) -> AppResult<Vec<u8>> {
        let term = blob_term(blob)?;
        let key = self
            .keys
            .get(&term)
            .ok_or_else(|| AppError::Internal(format!("no key for term {term}")))?;
        decrypt_with(key, path, blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_path_binding() {
        let b = Barrier::fresh();
        let blob = b.encrypt("kv/meta/a", b"hello");
        assert_eq!(b.decrypt("kv/meta/a", &blob).unwrap(), b"hello");
        // Same ciphertext moved to another path must not decrypt.
        assert!(b.decrypt("kv/meta/b", &blob).is_err());
    }

    #[test]
    fn old_terms_stay_readable_after_rotation() {
        let mut b = Barrier::fresh();
        let old = b.encrypt("p", b"v1");
        assert_eq!(b.rotate(), 2);
        let new = b.encrypt("p", b"v2");
        assert_eq!(blob_term(&old).unwrap(), 1);
        assert_eq!(blob_term(&new).unwrap(), 2);
        assert_eq!(b.decrypt("p", &old).unwrap(), b"v1");
        let b2 = Barrier::from_doc(b.to_doc()).unwrap();
        assert_eq!(b2.decrypt("p", &new).unwrap(), b"v2");
    }
}
