//! Raft RPC client: JSON over HTTP(S) to a peer's cluster port, every request
//! signed with the cluster HMAC key (see `cluster_auth`).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use openraft::error::{
    Fatal, NetworkError, RPCError, RaftError, RemoteError, ReplicationClosed, StreamingError, Unreachable,
};
use openraft::network::RPCOption;
use openraft::raft::{AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse};
use openraft::{RaftNetwork, RaftNetworkFactory, Snapshot, SnapshotMeta, Vote};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::cluster_auth;
use super::types::{ClusterNode, NodeId, TypeConfig};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Msg(String);

enum SendError {
    /// Could not connect — openraft backs off before retrying.
    Unreachable(Msg),
    /// Connected, but the exchange failed.
    Network(Msg),
}

#[derive(Serialize, Deserialize)]
pub struct FullSnapshotReq {
    pub vote: Vote<NodeId>,
    pub meta: SnapshotMeta<NodeId, ClusterNode>,
    /// base64 of the snapshot bytes.
    pub data: String,
}

#[derive(Clone)]
pub struct Network {
    pub client: reqwest::Client,
    pub key: Arc<[u8; 32]>,
}

impl RaftNetworkFactory<TypeConfig> for Network {
    type Network = PeerClient;

    async fn new_client(&mut self, target: NodeId, node: &ClusterNode) -> PeerClient {
        PeerClient {
            client: self.client.clone(),
            key: self.key.clone(),
            target,
            addr: node.cluster_addr.trim_end_matches('/').to_string(),
        }
    }
}

pub struct PeerClient {
    client: reqwest::Client,
    key: Arc<[u8; 32]>,
    target: NodeId,
    addr: String,
}

impl PeerClient {
    async fn post<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        req: &Req,
        ttl: Duration,
    ) -> Result<Resp, SendError> {
        let body = serde_json::to_vec(req).map_err(|e| SendError::Network(Msg(e.to_string())))?;
        let (ts, sig) = cluster_auth::sign(&self.key, path, &body);
        let res = self
            .client
            .post(format!("{}{}", self.addr, path))
            .header(cluster_auth::TS_HEADER, ts)
            .header(cluster_auth::SIG_HEADER, sig)
            .header("content-type", "application/json")
            .timeout(ttl)
            .body(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    SendError::Unreachable(Msg(e.to_string()))
                } else {
                    SendError::Network(Msg(e.to_string()))
                }
            })?;
        if !res.status().is_success() {
            // 503 = peer sealed / raft not running: treat as unreachable so we back off.
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            let m = Msg(format!("{} from {}: {}", status, self.addr, text));
            return Err(if status.as_u16() == 503 { SendError::Unreachable(m) } else { SendError::Network(m) });
        }
        res.json::<Resp>().await.map_err(|e| SendError::Network(Msg(e.to_string())))
    }
}

fn rpc_err<E: std::error::Error>(e: SendError) -> RPCError<NodeId, ClusterNode, E> {
    match e {
        SendError::Unreachable(m) => RPCError::Unreachable(Unreachable::new(&m)),
        SendError::Network(m) => RPCError::Network(NetworkError::new(&m)),
    }
}

impl RaftNetwork<TypeConfig> for PeerClient {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, ClusterNode, RaftError<NodeId>>> {
        let r: Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>> =
            self.post("/raft/append", &rpc, option.hard_ttl()).await.map_err(rpc_err)?;
        r.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, ClusterNode, RaftError<NodeId>>> {
        let r: Result<VoteResponse<NodeId>, RaftError<NodeId>> =
            self.post("/raft/vote", &rpc, option.hard_ttl()).await.map_err(rpc_err)?;
        r.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    /// The whole snapshot goes in one request — vault state is small (KBs–MBs).
    async fn full_snapshot(
        &mut self,
        vote: Vote<NodeId>,
        snapshot: Snapshot<TypeConfig>,
        cancel: impl Future<Output = ReplicationClosed> + Send + 'static,
        option: RPCOption,
    ) -> Result<SnapshotResponse<NodeId>, StreamingError<TypeConfig, Fatal<NodeId>>> {
        let req = FullSnapshotReq { vote, meta: snapshot.meta, data: B64.encode(snapshot.snapshot.as_slice()) };
        let send = self.post::<_, Result<SnapshotResponse<NodeId>, Fatal<NodeId>>>(
            "/raft/snapshot",
            &req,
            option.hard_ttl().max(Duration::from_secs(30)),
        );
        let r = tokio::select! {
            closed = cancel => return Err(StreamingError::Closed(closed)),
            r = send => r,
        };
        match r {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(fatal)) => Err(StreamingError::RemoteError(RemoteError::new(self.target, fatal))),
            Err(SendError::Unreachable(m)) => Err(StreamingError::Unreachable(Unreachable::new(&m))),
            Err(SendError::Network(m)) => Err(StreamingError::Network(NetworkError::new(&m))),
        }
    }
}
