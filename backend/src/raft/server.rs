//! The cluster listener (`CLUSTER_BIND_ADDR`, default :8201): receives Raft
//! RPCs from peers. Every request must carry a valid cluster HMAC, and the node
//! must be unsealed (Raft running) to answer at all.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use openraft::Snapshot;

use super::network::FullSnapshotReq;
use super::{cluster_auth, RaftBackend, Running};

pub fn router(backend: Arc<RaftBackend>) -> Router {
    Router::new()
        .route("/raft/append", post(append))
        .route("/raft/vote", post(vote))
        .route("/raft/snapshot", post(snapshot))
        .route("/raft/seal", post(seal))
        .route("/raft/write", post(write))
        .route("/raft/read-index", post(read_index))
        .route("/raft/membership", post(membership))
        .with_state(backend)
}

/// Resolve the running Raft instance and check the request signature.
async fn authorize(b: &RaftBackend, path: &str, headers: &HeaderMap, body: &[u8]) -> Result<Running, Response> {
    let Some(r) = b.running().await else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "node is sealed").into_response());
    };
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !cluster_auth::verify(&r.key, path, body, h(cluster_auth::TS_HEADER), h(cluster_auth::SIG_HEADER)) {
        tracing::warn!("rejected unauthenticated raft rpc on {path}");
        return Err((StatusCode::UNAUTHORIZED, "bad cluster signature").into_response());
    }
    Ok(r)
}

fn bad(e: impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST, e.to_string()).into_response()
}

async fn append(State(b): State<Arc<RaftBackend>>, headers: HeaderMap, body: Bytes) -> Response {
    let r = match authorize(&b, "/raft/append", &headers, &body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match serde_json::from_slice(&body) {
        Ok(req) => Json(r.raft.append_entries(req).await).into_response(),
        Err(e) => bad(e),
    }
}

async fn vote(State(b): State<Arc<RaftBackend>>, headers: HeaderMap, body: Bytes) -> Response {
    let r = match authorize(&b, "/raft/vote", &headers, &body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match serde_json::from_slice(&body) {
        Ok(req) => Json(r.raft.vote(req).await).into_response(),
        Err(e) => bad(e),
    }
}

async fn snapshot(State(b): State<Arc<RaftBackend>>, headers: HeaderMap, body: Bytes) -> Response {
    let r = match authorize(&b, "/raft/snapshot", &headers, &body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let req: FullSnapshotReq = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => return bad(e),
    };
    let data = match B64.decode(req.data) {
        Ok(d) => d,
        Err(e) => return bad(e),
    };
    let snap = Snapshot { meta: req.meta, snapshot: Box::new(data) };
    Json(r.raft.install_full_snapshot(req.vote, snap).await).into_response()
}

/// Seal-all from a peer: only a node holding the cluster key can ask.
async fn seal(State(b): State<Arc<RaftBackend>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(resp) = authorize(&b, "/raft/seal", &headers, &body).await {
        return resp;
    }
    tracing::warn!("seal requested by a cluster peer");
    b.seal_requested.notify_one();
    StatusCode::OK.into_response()
}

/// A follower forwarding a client write to us (the leader).
async fn write(State(b): State<Arc<RaftBackend>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(resp) = authorize(&b, "/raft/write", &headers, &body).await {
        return resp;
    }
    let req = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return bad(e),
    };
    match b.apply_forwarded(req).await {
        Ok(applied) => Json(applied).into_response(),
        Err(e) => e.into_response(),
    }
}

/// A follower asking how far it must catch up before serving a read.
async fn read_index(State(b): State<Arc<RaftBackend>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(resp) = authorize(&b, "/raft/read-index", &headers, &body).await {
        return resp;
    }
    match b.read_index().await {
        Ok(idx) => Json(idx).into_response(),
        Err(e) => e.into_response(),
    }
}

/// A follower forwarding a membership change (join / remove) to the leader.
async fn membership(State(b): State<Arc<RaftBackend>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(resp) = authorize(&b, "/raft/membership", &headers, &body).await {
        return resp;
    }
    let req: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return bad(e),
    };
    let res = if let Some(node) = req.get("add") {
        match serde_json::from_value(node.clone()) {
            Ok(node) => b.add_voter(node).await,
            Err(e) => return bad(e),
        }
    } else if let Some(name) = req.get("remove").and_then(|v| v.as_str()) {
        b.remove_peer(name).await
    } else {
        return bad("expected `add` or `remove`");
    };
    match res {
        Ok(()) => Json(true).into_response(),
        Err(e) => e.into_response(),
    }
}
