//! Auto-unseal: the root key is wrapped (encrypted) by an external key service
//! and stored next to the data. Every instance unwraps it at boot, so replicas
//! that scale out, roll or restart come up unsealed with no operator.
//!
//! Trust moves to the key service: whoever can call its *decrypt* with timika's
//! credentials can unseal. Deleting or disabling the key there makes the vault
//! permanently unreadable — that is the same property as losing Shamir shares.
//!
//! | SEAL_TYPE | wraps with                              |
//! |-----------|-----------------------------------------|
//! | shamir    | nothing — operators hold key shares      |
//! | transit   | HashiCorp Vault / OpenBao Transit engine |
//! | awskms    | AWS KMS                                  |
//! | static    | a 32-byte key file (e.g. a k8s/Swarm secret) — lowest assurance |

pub mod awskms;
pub mod static_key;
pub mod transit;

use serde::{Deserialize, Serialize};

/// The wrapped root key as stored at `core/root-key-wrapped` (plaintext storage:
/// the ciphertext is only useful to someone who can call the key service).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WrappedKey {
    pub seal_type: String,
    /// Which key wrapped it (transit key name, KMS key ARN, static key fingerprint).
    pub key_id: String,
    pub ciphertext: String,
}

pub enum AutoSeal {
    Transit(transit::Transit),
    AwsKms(awskms::AwsKms),
    Static(static_key::StaticKey),
}

impl AutoSeal {
    pub fn kind(&self) -> &'static str {
        match self {
            AutoSeal::Transit(_) => "transit",
            AutoSeal::AwsKms(_) => "awskms",
            AutoSeal::Static(_) => "static",
        }
    }

    pub async fn wrap(&self, root_key: &[u8]) -> anyhow::Result<WrappedKey> {
        let (key_id, ciphertext) = match self {
            AutoSeal::Transit(t) => t.encrypt(root_key).await?,
            AutoSeal::AwsKms(k) => k.encrypt(root_key).await?,
            AutoSeal::Static(s) => s.encrypt(root_key)?,
        };
        Ok(WrappedKey { seal_type: self.kind().into(), key_id, ciphertext })
    }

    pub async fn unwrap(&self, w: &WrappedKey) -> anyhow::Result<Vec<u8>> {
        if w.seal_type != self.kind() {
            anyhow::bail!(
                "root key was wrapped by `{}` but this instance is configured with SEAL_TYPE={}",
                w.seal_type,
                self.kind()
            );
        }
        match self {
            AutoSeal::Transit(t) => t.decrypt(&w.ciphertext).await,
            AutoSeal::AwsKms(k) => k.decrypt(&w.ciphertext).await,
            AutoSeal::Static(s) => s.decrypt(&w.ciphertext),
        }
    }

    /// Human-readable target, for status and logs.
    pub fn describe(&self) -> String {
        match self {
            AutoSeal::Transit(t) => t.describe(),
            AutoSeal::AwsKms(k) => k.describe(),
            AutoSeal::Static(s) => s.describe(),
        }
    }

    pub fn from_env(http: &reqwest::Client) -> anyhow::Result<Option<Self>> {
        let kind = std::env::var("SEAL_TYPE").unwrap_or_else(|_| "shamir".into()).to_ascii_lowercase();
        Ok(match kind.as_str() {
            "shamir" | "" => None,
            "transit" => Some(AutoSeal::Transit(transit::Transit::from_env()?)),
            "awskms" => Some(AutoSeal::AwsKms(awskms::AwsKms::from_env(http.clone())?)),
            "static" => Some(AutoSeal::Static(static_key::StaticKey::from_env()?)),
            other => {
                // Never echo much of a bad value back: a mangled env line can carry secrets.
                let shown: String = other.chars().take(12).collect();
                anyhow::bail!("SEAL_TYPE must be shamir, transit, awskms or static — got `{shown}…`")
            }
        })
    }
}

pub(crate) fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

pub(crate) fn require(key: &str) -> anyhow::Result<String> {
    env(key).ok_or_else(|| anyhow::anyhow!("{key} is required for this SEAL_TYPE"))
}
