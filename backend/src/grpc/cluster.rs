use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tokio_stream::Stream;
use tonic::{Request, Status};

use super::convert::str_opt;
use super::pb::cluster_service_server::ClusterService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::error::{AppError, AppResult};
use crate::raft::types::ClusterNode;
use crate::raft::RaftBackend;

impl Ctx {
    fn raft(&self) -> AppResult<&Arc<RaftBackend>> {
        self.st
            .core
            .storage
            .raft()
            .ok_or_else(|| AppError::BadRequest("storage backend is not raft (set STORAGE=raft)".into()))
    }

    async fn peers(&self) -> AppResult<pb::PeersResponse> {
        let peers = self.raft()?.peers().await;
        Ok(pb::PeersResponse { peers: peers.iter().map(peer_pb).collect() })
    }
}

fn peer_pb(p: &Value) -> pb::Peer {
    pb::Peer {
        name: str_opt(&p["name"]).unwrap_or_default(),
        api_addr: str_opt(&p["api_addr"]).unwrap_or_default(),
        cluster_addr: str_opt(&p["cluster_addr"]).unwrap_or_default(),
        voter: p["voter"] == true,
        leader: p["leader"] == true,
        self_: p["self"] == true,
    }
}

fn node_of(n: Option<pb::ClusterNode>) -> AppResult<ClusterNode> {
    let n = n.ok_or_else(|| AppError::BadRequest("node is required".into()))?;
    let ok_name = !n.name.is_empty()
        && n.name.len() <= 63
        && n.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    let ok_addr = |a: &str| a.starts_with("http://") || a.starts_with("https://");
    if !ok_name || !ok_addr(&n.api_addr) || !ok_addr(&n.cluster_addr) {
        return Err(AppError::BadRequest("invalid node descriptor".into()));
    }
    Ok(ClusterNode { name: n.name, api_addr: n.api_addr, cluster_addr: n.cluster_addr })
}

const SNAPSHOT_CHUNK: usize = 256 * 1024;

type SnapStream = Pin<Box<dyn Stream<Item = Result<pb::SnapshotChunk, Status>> + Send>>;

#[tonic::async_trait]
impl ClusterService for Ctx {
    async fn configuration(&self, req: Request<()>) -> Reply<pb::PeersResponse> {
        let (me, _) = self.who(&req, Need::Read).await?;
        Ok(reply(self.peers().await?, &me))
    }

    async fn remove_peer(&self, req: Request<pb::RemovePeerRequest>) -> Reply<pb::PeersResponse> {
        crate::audit::target(format!("peer {}", req.get_ref().node_id));
        let (me, _) = self.who(&req, Need::Admin).await?;
        self.raft()?.remove_peer(&req.get_ref().node_id).await?;
        Ok(reply(self.peers().await?, &me))
    }

    type SnapshotStream = SnapStream;

    async fn snapshot(&self, req: Request<()>) -> Reply<Self::SnapshotStream> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let bytes = self.raft()?.snapshot_bytes()?;
        let chunks: Vec<Result<pb::SnapshotChunk, Status>> =
            bytes.chunks(SNAPSHOT_CHUNK).map(|c| Ok(pb::SnapshotChunk { data: c.to_vec() })).collect();
        Ok(reply(Box::pin(tokio_stream::iter(chunks)) as SnapStream, &me))
    }

    /// Public: the joiner has no token yet. The challenge itself is the proof.
    async fn join_challenge(&self, req: Request<pb::JoinChallengeRequest>) -> Reply<pb::JoinChallengeResponse> {
        self.raft()?;
        let node = node_of(req.into_inner().node)?;
        let c = self.st.core.join_challenge(&node).await?;
        Ok(tonic::Response::new(pb::JoinChallengeResponse {
            seal_config: to_json(&c.seal_config)?,
            keyring: c.keyring,
            challenge: c.challenge,
            wrapped_root: c.wrapped_root.as_ref().map(to_json).transpose()?,
        }))
    }

    async fn join_answer(&self, req: Request<pb::JoinAnswerRequest>) -> Reply<pb::PeersResponse> {
        self.raft()?;
        let r = req.into_inner();
        let node = node_of(r.node)?;
        self.st.core.join_answer(node, &r.answer).await?;
        Ok(tonic::Response::new(self.peers().await?))
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<String, Status> {
    serde_json::to_string(v).map_err(|_| Status::internal("internal error"))
}
