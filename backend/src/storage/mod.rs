//! Physical storage — pluggable, and deliberately dumb.
//!
//! Both backends move opaque byte blobs keyed by a path and know nothing about
//! encryption: everything except `core/seal-config` arrives already encrypted
//! by the barrier. Five operations are all the vault needs:
//! get · commit(puts, deletes) atomically · list(prefix) · ping · describe.
//!
//! | backend | use for                    | HA | persistence                    |
//! |---------|----------------------------|----|--------------------------------|
//! | redis   | speed; standalone / Sentinel / Cluster | via Redis | Redis AOF/RDB (+ WAIT) |
//! | raft    | production (k8s or VMs)    | yes| per-node redb + Raft quorum    |

pub mod redis;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppResult;
use crate::raft::RaftBackend;
pub use redis::RedisStorage;

/// Optimistic-concurrency precondition for a commit: the blob stored at `path`
/// must still be exactly `expect` (`None` = the key must not exist). Checked
/// atomically with the write, so N timika instances sharing storage can never
/// both "win" a read-modify-write — the loser gets `AppError::WriteConflict`
/// and retries on fresh data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Guard {
    pub path: String,
    #[serde(with = "crate::raft::types::b64_opt")]
    pub expect: Option<Vec<u8>>,
}

#[derive(Clone)]
pub enum Storage {
    Redis(Arc<RedisStorage>),
    Raft(Arc<RaftBackend>),
}

impl Storage {
    pub fn kind(&self) -> &'static str {
        match self {
            Storage::Redis(_) => "redis",
            Storage::Raft(_) => "raft",
        }
    }

    pub fn raft(&self) -> Option<&Arc<RaftBackend>> {
        match self {
            Storage::Raft(r) => Some(r),
            Storage::Redis(_) => None,
        }
    }

    /// Consistent read (on a Raft leader, confirmed with a quorum).
    pub async fn get(&self, path: &str) -> AppResult<Option<Vec<u8>>> {
        match self {
            Storage::Redis(r) => r.get(path).await,
            Storage::Raft(r) => r.get(path).await,
        }
    }

    /// This node's own copy — the only read possible while sealed, and what
    /// unseal uses to find the keyring.
    pub async fn get_local(&self, path: &str) -> AppResult<Option<Vec<u8>>> {
        match self {
            Storage::Redis(r) => r.get(path).await,
            Storage::Raft(r) => r.get_local(path),
        }
    }

    /// Apply puts and deletes atomically (one Redis Lua script / one Raft log
    /// entry) — only if every guard still holds.
    pub async fn commit(&self, puts: &[(String, Vec<u8>)], deletes: &[String], guards: &[Guard]) -> AppResult<()> {
        match self {
            Storage::Redis(r) => r.commit(puts, deletes, guards).await,
            Storage::Raft(r) => r.commit(puts, deletes, guards).await,
        }
    }

    pub async fn list(&self, prefix: &str) -> AppResult<Vec<String>> {
        match self {
            Storage::Redis(r) => r.list(prefix).await,
            Storage::Raft(r) => r.list(prefix),
        }
    }

    pub async fn ping(&self) -> AppResult<()> {
        match self {
            Storage::Redis(r) => r.ping().await,
            Storage::Raft(_) => Ok(()),
        }
    }

    pub async fn describe(&self) -> AppResult<Value> {
        match self {
            Storage::Redis(r) => r.describe().await,
            Storage::Raft(r) => r.describe().await,
        }
    }
}
