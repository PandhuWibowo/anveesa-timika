//! Integrated Raft storage (Vault's "integrated storage", in Rust).
//!
//! Every node keeps a full copy of the vault's key space in a local redb file.
//! Writes go through the Raft leader and are applied on every node once a
//! majority has them on disk. Like Vault, a node takes part in Raft **only while
//! unsealed**: the cluster auth key is derived from the root key, so a sealed
//! node has no way to talk to its peers — and neither does anyone who does not
//! hold the unseal keys.

pub mod network;
pub mod server;
pub mod store;
pub mod types;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openraft::{ChangeMembers, Config, Raft, ServerState, SnapshotPolicy};
use redb::Database;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::error::{AppError, AppResult};
use crate::storage::Guard;
use network::Network;
use store::{LogStore, StateMachine};
use types::{node_id, ClusterNode, NodeId, Request, TypeConfig};

pub struct RaftConfig {
    pub node: ClusterNode,
    pub path: PathBuf,
    /// API addresses of peers to try joining through (`retry_join`).
    pub retry_join: Vec<String>,
    pub snapshot_threshold: u64,
    pub trailing_logs: u64,
}

#[derive(Clone)]
pub struct Running {
    pub raft: Raft<TypeConfig>,
    pub key: Arc<[u8; 32]>,
}

pub struct RaftBackend {
    pub cfg: RaftConfig,
    /// Fired when a peer asks this node to seal (seal-all); `main` wires it to `Core::seal`.
    pub seal_requested: tokio::sync::Notify,
    /// Raft allows one membership change at a time; joins that arrive together
    /// (e.g. `unseal --all`, or auto-unseal) queue here instead of failing.
    membership_lock: tokio::sync::Mutex<()>,
    pub id: NodeId,
    db: Arc<Database>,
    client: reqwest::Client,
    running: RwLock<Option<Running>>,
}

fn internal<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Internal(e.to_string())
}

impl RaftBackend {
    pub fn open(cfg: RaftConfig, client: reqwest::Client) -> anyhow::Result<Self> {
        let db = store::open(&cfg.path)?;
        Ok(Self {
            id: node_id(&cfg.node.name),
            cfg,
            seal_requested: tokio::sync::Notify::new(),
            membership_lock: tokio::sync::Mutex::new(()),
            db,
            client,
            running: RwLock::new(None),
        })
    }

    pub async fn running(&self) -> Option<Running> {
        self.running.read().await.clone()
    }

    /// Has this node ever been bootstrapped or joined a cluster?
    pub fn is_member(&self) -> bool {
        store::has_state(&self.db).unwrap_or(false)
    }

    // ── lifecycle ────────────────────────────────────────────────────────────

    /// Start participating in Raft (called at unseal).
    pub async fn start(&self, key: [u8; 32]) -> AppResult<()> {
        let mut guard = self.running.write().await;
        if guard.is_some() {
            return Ok(());
        }
        let config = Config {
            cluster_name: "timika".into(),
            heartbeat_interval: 250,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            snapshot_policy: SnapshotPolicy::LogsSinceLast(self.cfg.snapshot_threshold),
            max_in_snapshot_log_to_keep: self.cfg.trailing_logs,
            ..Default::default()
        }
        .validate()
        .map_err(internal)?;
        let key = Arc::new(key);
        let network = Network { client: self.client.clone(), key: key.clone() };
        let raft = Raft::new(
            self.id,
            Arc::new(config),
            network,
            LogStore::new(self.db.clone()),
            StateMachine::new(self.db.clone()),
        )
        .await
        .map_err(internal)?;
        *guard = Some(Running { raft, key });
        tracing::info!("raft started as {} (id {})", self.cfg.node.name, self.id);
        Ok(())
    }

    /// Stop participating (called at seal). Local data stays on disk.
    pub async fn stop(&self) {
        if let Some(r) = self.running.write().await.take() {
            let _ = r.raft.shutdown().await;
            tracing::info!("raft stopped");
        }
    }

    /// Form a brand-new single-voter cluster with this node (called by `sys/init`).
    pub async fn bootstrap(&self, key: [u8; 32]) -> AppResult<()> {
        if self.is_member() {
            return Err(AppError::BadRequest("this node already holds raft state".into()));
        }
        self.start(key).await?;
        let r = self.running().await.ok_or(AppError::Sealed)?;
        r.raft
            .initialize(BTreeMap::from([(self.id, self.cfg.node.clone())]))
            .await
            .map_err(internal)?;
        r.raft
            .wait(Some(Duration::from_secs(10)))
            .current_leader(self.id, "bootstrap")
            .await
            .map_err(internal)?;
        Ok(())
    }

    // ── data path ────────────────────────────────────────────────────────────

    pub fn get_local(&self, path: &str) -> AppResult<Option<Vec<u8>>> {
        store::data_get(&self.db, path).map_err(AppError::Internal)
    }

    /// Reads are always local; `linearize` (once per request) makes them current.
    pub async fn get(&self, path: &str) -> AppResult<Option<Vec<u8>>> {
        self.get_local(path)
    }

    /// Wait until this node has applied every write committed before now.
    /// Leader: confirm leadership with a quorum. Follower: ask the leader for
    /// its read index (signed cluster RPC) and wait to catch up to it — so any
    /// node can serve reads that never miss a completed write.
    pub async fn linearize(&self) -> AppResult<()> {
        let Some(r) = self.running().await else { return Ok(()) };
        let m = r.raft.metrics().borrow().clone();
        match m.current_leader {
            Some(l) if l == self.id => {
                r.raft.ensure_linearizable().await.map_err(|e| AppError::Unavailable(e.to_string()))?;
            }
            Some(l) => {
                let leader = m
                    .membership_config
                    .membership()
                    .get_node(&l)
                    .cloned()
                    .ok_or_else(|| AppError::Unavailable("leader not in membership".into()))?;
                let idx: Option<u64> = self.cluster_call(&r, &leader, "/raft/read-index", &serde_json::json!({})).await?;
                if let Some(idx) = idx {
                    r.raft
                        .wait(Some(Duration::from_secs(5)))
                        .applied_index_at_least(Some(idx), "read-index")
                        .await
                        .map_err(|e| AppError::Unavailable(format!("catching up with the leader: {e}")))?;
                }
            }
            None => return Err(AppError::Unavailable("no raft leader elected — cluster may have lost quorum".into())),
        }
        Ok(())
    }

    /// Leader side of `linearize`.
    pub async fn read_index(&self) -> AppResult<Option<u64>> {
        let r = self.running().await.ok_or(AppError::Sealed)?;
        r.raft
            .ensure_linearizable()
            .await
            .map(|l| l.map(|l| l.index))
            .map_err(|e| AppError::Unavailable(format!("not the leader: {e}")))
    }

    /// Signed JSON call to another node's cluster listener.
    async fn cluster_call<T: serde::de::DeserializeOwned>(
        &self,
        r: &Running,
        to: &ClusterNode,
        path: &str,
        body: &serde_json::Value,
    ) -> AppResult<T> {
        let body = serde_json::to_vec(body)?;
        let (ts, sig) = cluster_auth::sign(&r.key, path, &body);
        let res = self
            .client
            .post(format!("{}{path}", to.cluster_addr.trim_end_matches('/')))
            .header(cluster_auth::TS_HEADER, ts)
            .header(cluster_auth::SIG_HEADER, sig)
            .header("content-type", "application/json")
            .body(body)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| AppError::Unavailable(format!("{} unreachable: {e}", to.name)))?;
        if !res.status().is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(AppError::Unavailable(format!("{} refused {path}: {text}", to.name)));
        }
        res.json::<T>().await.map_err(|e| AppError::Unavailable(e.to_string()))
    }

    fn leader_node(&self, r: &Running) -> AppResult<Option<ClusterNode>> {
        let m = r.raft.metrics().borrow().clone();
        match m.current_leader {
            Some(l) if l == self.id => Ok(None),
            Some(l) => Ok(m.membership_config.membership().get_node(&l).cloned()),
            None => Err(AppError::Unavailable("no raft leader elected".into())),
        }
    }

    pub fn list(&self, prefix: &str) -> AppResult<Vec<String>> {
        store::data_list(&self.db, prefix).map_err(AppError::Internal)
    }

    /// Commit through Raft. On a follower the write is forwarded to the leader
    /// over the signed cluster channel, so node-local work (e.g. a bastion
    /// session on a follower recording its output) can write too.
    pub async fn commit(&self, puts: &[(String, Vec<u8>)], deletes: &[String], guards: &[Guard]) -> AppResult<()> {
        let r = self.running().await.ok_or(AppError::Sealed)?;
        let req = Request::Commit { puts: puts.to_vec(), deletes: deletes.to_vec(), guards: guards.to_vec() };
        let applied = match r.raft.client_write(req.clone()).await {
            Ok(resp) => resp.data,
            Err(e) => match e.forward_to_leader() {
                Some(f) => {
                    let leader = f.leader_node.clone().ok_or_else(|| AppError::Unavailable("no raft leader elected".into()))?;
                    self.forward_write(&r, &leader, &req).await?
                }
                None => return Err(internal(e)),
            },
        };
        if !applied {
            return Err(AppError::WriteConflict);
        }
        Ok(())
    }

    async fn forward_write(&self, r: &Running, leader: &ClusterNode, req: &Request) -> AppResult<bool> {
        let body = serde_json::to_vec(req)?;
        let (ts, sig) = cluster_auth::sign(&r.key, "/raft/write", &body);
        let res = self
            .client
            .post(format!("{}/raft/write", leader.cluster_addr.trim_end_matches('/')))
            .header(cluster_auth::TS_HEADER, ts)
            .header(cluster_auth::SIG_HEADER, sig)
            .header("content-type", "application/json")
            .body(body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| AppError::Unavailable(format!("forwarding write to leader {}: {e}", leader.name)))?;
        if !res.status().is_success() {
            return Err(AppError::Unavailable(format!("leader {} refused the write ({})", leader.name, res.status())));
        }
        res.json::<bool>().await.map_err(|e| AppError::Unavailable(e.to_string()))
    }

    /// Leader side of `forward_write`.
    pub async fn apply_forwarded(&self, req: Request) -> AppResult<bool> {
        let r = self.running().await.ok_or(AppError::Sealed)?;
        r.raft
            .client_write(req)
            .await
            .map(|resp| resp.data)
            .map_err(|e| AppError::Unavailable(format!("not the leader any more: {e}")))
    }

    // ── cluster ──────────────────────────────────────────────────────────────

    pub async fn is_leader(&self) -> bool {
        matches!(self.running().await, Some(r) if r.raft.metrics().borrow().current_leader == Some(self.id))
    }

    /// Add a node as a voter: replicate to it as a learner until caught up,
    /// then promote it. Must run on the leader.
    pub async fn add_voter(&self, node: ClusterNode) -> AppResult<()> {
        let r = self.running().await.ok_or(AppError::Sealed)?;
        // Membership changes only happen on the leader: forward if we aren't it.
        if let Some(leader) = self.leader_node(&r)? {
            let _: bool = self.cluster_call(&r, &leader, "/raft/membership", &serde_json::json!({ "add": node })).await?;
            return Ok(());
        }
        let id = node_id(&node.name);
        let name = node.name.clone();
        let _one_at_a_time = self.membership_lock.lock().await;
        r.raft.add_learner(id, node, true).await.map_err(internal)?;
        // A change still settling (the previous join's joint config) is retried,
        // not reported as a failed join.
        for attempt in 0.. {
            match r.raft.change_membership(ChangeMembers::AddVoterIds(BTreeSet::from([id])), false).await {
                Ok(_) => break,
                Err(e) if attempt < 50 && e.to_string().contains("undergoing a configuration change") => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(internal(e)),
            }
        }
        tracing::info!("raft: {name} joined as voter");
        Ok(())
    }

    pub async fn remove_peer(&self, name: &str) -> AppResult<()> {
        let r = self.running().await.ok_or(AppError::Sealed)?;
        if let Some(leader) = self.leader_node(&r)? {
            let _: bool = self.cluster_call(&r, &leader, "/raft/membership", &serde_json::json!({ "remove": name })).await?;
            return Ok(());
        }
        let id = node_id(name);
        if id == self.id {
            return Err(AppError::BadRequest(
                "refusing to remove the current leader; seal it or step it down first".into(),
            ));
        }
        let m = r.raft.metrics().borrow().clone();
        if m.membership_config.membership().get_node(&id).is_none() {
            return Err(AppError::NotFound(format!("peer `{name}`")));
        }
        let _one_at_a_time = self.membership_lock.lock().await;
        r.raft
            .change_membership(ChangeMembers::RemoveVoters(BTreeSet::from([id])), false)
            .await
            .map_err(internal)?;
        tracing::info!("raft: removed peer {name}");
        Ok(())
    }

    /// Ask every other member to seal (signed with the cluster key). Best effort:
    /// unreachable or already-sealed peers are reported, not retried.
    pub async fn broadcast_seal(&self) -> Vec<Value> {
        let Some(r) = self.running().await else { return vec![] };
        let m = r.raft.metrics().borrow().clone();
        let peers: Vec<ClusterNode> = m
            .membership_config
            .membership()
            .nodes()
            .filter(|(id, _)| **id != self.id)
            .map(|(_, n)| n.clone())
            .collect();
        let mut out = Vec::new();
        for n in peers {
            let (ts, sig) = cluster_auth::sign(&r.key, "/raft/seal", b"{}");
            let res = self
                .client
                .post(format!("{}/raft/seal", n.cluster_addr.trim_end_matches('/')))
                .header(cluster_auth::TS_HEADER, ts)
                .header(cluster_auth::SIG_HEADER, sig)
                .header("content-type", "application/json")
                .body("{}")
                .timeout(Duration::from_secs(5))
                .send()
                .await;
            let ok = matches!(&res, Ok(r) if r.status().is_success());
            out.push(json!({ "name": n.name, "sealed": ok }));
        }
        out
    }

    /// Membership as seen by this node: live metrics when running, the last
    /// applied membership from disk when sealed.
    pub async fn peers(&self) -> Vec<Value> {
        let (membership, leader) = match self.running().await {
            Some(r) => {
                let m = r.raft.metrics().borrow().clone();
                (Some(m.membership_config.membership().clone()), m.current_leader)
            }
            None => (
                store::applied_membership(&self.db).ok().flatten().map(|m| m.membership().clone()),
                None,
            ),
        };
        let Some(membership) = membership else { return vec![] };
        let voters: BTreeSet<NodeId> = membership.voter_ids().collect();
        membership
            .nodes()
            .map(|(id, n)| {
                json!({
                    "name": n.name,
                    "api_addr": n.api_addr,
                    "cluster_addr": n.cluster_addr,
                    "voter": voters.contains(id),
                    "leader": leader == Some(*id),
                    "self": *id == self.id,
                })
            })
            .collect()
    }

    pub async fn describe(&self) -> AppResult<Value> {
        let running = self.running().await;
        let metrics = running.as_ref().map(|r| r.raft.metrics().borrow().clone());
        let peers = self.peers().await;
        let voters = peers.iter().filter(|p| p["voter"] == true).count();
        let leader = peers.iter().find(|p| p["leader"] == true).cloned();

        let mut warnings: Vec<String> = Vec::new();
        if running.is_none() {
            warnings.push("Raft is not running on this node (sealed) — it is not replicating.".into());
        }
        if voters == 1 {
            warnings.push("Single voter: no fault tolerance. Join 2 more nodes for a 3-node cluster.".into());
        } else if voters > 0 && voters % 2 == 0 {
            warnings.push(format!(
                "{voters} voters survive no more failures than {} would — use an odd number of voters.",
                voters - 1
            ));
        }
        if running.is_some() && leader.is_none() {
            warnings.push("No leader elected — the cluster may have lost quorum.".into());
        }

        let state = metrics.as_ref().map(|m| match m.state {
            ServerState::Leader => "leader",
            ServerState::Follower => "follower",
            ServerState::Candidate => "candidate",
            ServerState::Learner => "learner",
            ServerState::Shutdown => "shutdown",
        });
        Ok(json!({
            "engine": "raft",
            "node": {
                "name": self.cfg.node.name,
                "id": self.id.to_string(),
                "api_addr": self.cfg.node.api_addr,
                "cluster_addr": self.cfg.node.cluster_addr,
            },
            "path": self.cfg.path.display().to_string(),
            "running": running.is_some(),
            "state": state,
            "term": metrics.as_ref().map(|m| m.current_term),
            "last_log_index": metrics.as_ref().and_then(|m| m.last_log_index),
            "last_applied": metrics.as_ref().and_then(|m| m.last_applied.map(|l| l.index)),
            "snapshot_index": metrics.as_ref().and_then(|m| m.snapshot.map(|l| l.index)),
            "leader": leader,
            "voters": voters,
            "peers": peers,
            "keys": store::data_len(&self.db).map_err(AppError::Internal)?,
            "warnings": warnings,
        }))
    }

    /// Backup: a point-in-time copy of the whole (encrypted) key space.
    pub fn snapshot_bytes(&self) -> AppResult<Vec<u8>> {
        let entries = store::data_dump(&self.db).map_err(AppError::Internal)?;
        let doc = json!({
            "format": "timika-raft-snapshot/v1",
            "node": self.cfg.node.name,
            "taken_at": chrono::Utc::now(),
            "data": serde_json::to_value(types::SnapshotData { entries })?,
        });
        Ok(serde_json::to_vec(&doc)?)
    }
}

/// HMAC authentication for node-to-node Raft RPCs.
///
/// The key is derived from the root key, so only nodes that have been unsealed
/// with the cluster's unseal keys can produce or verify signatures.
pub mod cluster_auth {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    pub const TS_HEADER: &str = "x-timika-cluster-ts";
    pub const SIG_HEADER: &str = "x-timika-cluster-sig";
    const MAX_SKEW_SECS: i64 = 60;

    type H = Hmac<Sha256>;

    pub fn derive_key(root_key: &[u8; 32]) -> [u8; 32] {
        let mut mac = H::new_from_slice(root_key).expect("any key length");
        mac.update(b"timika raft cluster key v1");
        mac.finalize().into_bytes().into()
    }

    fn mac(key: &[u8; 32], ts: &str, path: &str, body: &[u8]) -> H {
        let mut mac = H::new_from_slice(key).expect("any key length");
        mac.update(ts.as_bytes());
        mac.update(b"\n");
        mac.update(path.as_bytes());
        mac.update(b"\n");
        mac.update(body);
        mac
    }

    pub fn sign(key: &[u8; 32], path: &str, body: &[u8]) -> (String, String) {
        let ts = chrono::Utc::now().timestamp().to_string();
        let sig = hex::encode(mac(key, &ts, path, body).finalize().into_bytes());
        (ts, sig)
    }

    /// HMAC tag over an arbitrary message (e.g. the seal-all marker).
    pub fn tag(key: &[u8; 32], msg: &[u8]) -> String {
        let mut mac = H::new_from_slice(key).expect("any key length");
        mac.update(msg);
        hex::encode(mac.finalize().into_bytes())
    }

    pub fn check(key: &[u8; 32], msg: &[u8], tag: &str) -> bool {
        let Ok(t) = hex::decode(tag) else { return false };
        let mut mac = H::new_from_slice(key).expect("any key length");
        mac.update(msg);
        mac.verify_slice(&t).is_ok()
    }

    pub fn verify(key: &[u8; 32], path: &str, body: &[u8], ts: &str, sig: &str) -> bool {
        let Ok(t) = ts.parse::<i64>() else { return false };
        if (chrono::Utc::now().timestamp() - t).abs() > MAX_SKEW_SECS {
            return false;
        }
        let Ok(sig) = hex::decode(sig) else { return false };
        mac(key, ts, path, body).verify_slice(&sig).is_ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn sign_verify() {
            let k = derive_key(&[7u8; 32]);
            let (ts, sig) = sign(&k, "/raft/vote", b"{}");
            assert!(verify(&k, "/raft/vote", b"{}", &ts, &sig));
            assert!(!verify(&k, "/raft/append", b"{}", &ts, &sig));
            assert!(!verify(&derive_key(&[8u8; 32]), "/raft/vote", b"{}", &ts, &sig));
        }
    }
}
