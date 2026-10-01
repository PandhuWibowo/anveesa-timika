//! Raft type configuration: what a log entry carries and what a node is.

use std::fmt;
use std::io::Cursor;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::storage::Guard;

pub type NodeId = u64;

/// The only state-machine command: an atomic batch of puts and deletes — the
/// same shape as a Redis MULTI in the Redis backend. Values are ciphertext
/// produced by the barrier on the leader, so the log never holds plaintext.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Commit {
        #[serde(with = "b64_pairs")]
        puts: Vec<(String, Vec<u8>)>,
        deletes: Vec<String>,
        /// Applied only if all hold; otherwise the entry is a no-op and the
        /// response is `false` (see `storage::Guard`).
        #[serde(default)]
        guards: Vec<Guard>,
    },
}

/// A cluster member as recorded in Raft membership.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterNode {
    /// Human name, e.g. `timika-0` (the pod name on Kubernetes).
    pub name: String,
    /// Where clients (and forwarded requests) reach this node's `/v1` API.
    pub api_addr: String,
    /// Where peers send Raft RPCs.
    pub cluster_addr: String,
}

impl fmt::Display for ClusterNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.name, self.cluster_addr)
    }
}

/// Stable numeric Raft id derived from the node name, so operators only ever
/// deal with names.
pub fn node_id(name: &str) -> NodeId {
    let h = Sha256::digest(name.as_bytes());
    u64::from_be_bytes(h[..8].try_into().unwrap())
}

openraft::declare_raft_types!(
    pub TypeConfig:
        D = Request,
        // `true` = applied, `false` = a guard failed.
        R = bool,
        NodeId = NodeId,
        Node = ClusterNode,
        SnapshotData = Vec<u8>,
);

// `declare_raft_types!` names `Cursor` in its defaults even when overridden.
#[allow(dead_code)]
type _Cursor = Cursor<Vec<u8>>;

/// Snapshot payload: the full state machine key space.
#[derive(Serialize, Deserialize, Default)]
pub struct SnapshotData {
    #[serde(with = "b64_pairs")]
    pub entries: Vec<(String, Vec<u8>)>,
}

/// serde helper: `Vec<(String, Vec<u8>)>` with base64 values, so JSON stays compact.
pub mod b64_pairs {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[(String, Vec<u8>)], s: S) -> Result<S::Ok, S::Error> {
        v.iter()
            .map(|(k, b)| (k.as_str(), B64.encode(b)))
            .collect::<Vec<_>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(String, Vec<u8>)>, D::Error> {
        Vec::<(String, String)>::deserialize(d)?
            .into_iter()
            .map(|(k, b)| B64.decode(b).map(|b| (k, b)).map_err(serde::de::Error::custom))
            .collect()
    }
}

/// serde helper: `Option<Vec<u8>>` as optional base64.
pub mod b64_opt {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().map(|b| B64.encode(b)).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|b| B64.decode(b).map_err(serde::de::Error::custom))
            .transpose()
    }
}
