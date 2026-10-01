//! A static 32-byte key as the "key service" — for platforms without a KMS.
//!
//! SEAL_STATIC_KEY_FILE   file holding the key (base64 or hex), e.g. a mounted
//!                        Kubernetes Secret or Docker Swarm secret, or
//! SEAL_STATIC_KEY        the key itself (base64 or hex)
//!
//! LOWEST ASSURANCE: anyone who can read that secret *and* the storage can
//! decrypt everything. Keep the secret out of backups of the data, and prefer
//! `transit` or `awskms` where available. Generate one with:
//!   head -c 32 /dev/urandom | base64

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::env;
use crate::barrier;

const AAD: &str = "core/root-key-wrapped/static";

pub struct StaticKey {
    key: Zeroizing<[u8; 32]>,
    id: String,
}

impl StaticKey {
    pub fn from_env() -> anyhow::Result<Self> {
        let raw = match (env("SEAL_STATIC_KEY_FILE"), env("SEAL_STATIC_KEY")) {
            (Some(f), _) => std::fs::read_to_string(f)?,
            (None, Some(k)) => k,
            _ => anyhow::bail!("SEAL_TYPE=static needs SEAL_STATIC_KEY_FILE or SEAL_STATIC_KEY"),
        };
        Self::parse(raw.trim())
    }

    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let bytes = Zeroizing::new(
            B64.decode(s)
                .ok()
                .filter(|b| b.len() == 32)
                .or_else(|| hex::decode(s).ok().filter(|b| b.len() == 32))
                .ok_or_else(|| anyhow::anyhow!("static seal key must be 32 bytes, base64 or hex"))?,
        );
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&bytes);
        // Fingerprint only — never the key.
        let id = format!("static:{}", &hex::encode(Sha256::digest(key.as_slice()))[..16]);
        Ok(Self { key, id })
    }

    pub fn describe(&self) -> String {
        format!("static key {}", self.id)
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> anyhow::Result<(String, String)> {
        Ok((self.id.clone(), B64.encode(barrier::encrypt_with(&self.key, 0, AAD, plaintext))))
    }

    pub fn decrypt(&self, ciphertext: &str) -> anyhow::Result<Vec<u8>> {
        let blob = B64.decode(ciphertext)?;
        barrier::decrypt_with(&self.key, AAD, &blob)
            .map_err(|_| anyhow::anyhow!("static seal key does not match the one the vault was initialized with"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrong_key() {
        let a = StaticKey::parse(&B64.encode([1u8; 32])).unwrap();
        let b = StaticKey::parse(&hex::encode([2u8; 32])).unwrap();
        let (_, ct) = a.encrypt(b"root").unwrap();
        assert_eq!(a.decrypt(&ct).unwrap(), b"root");
        assert!(b.decrypt(&ct).is_err());
        assert!(StaticKey::parse("short").is_err());
    }
}
