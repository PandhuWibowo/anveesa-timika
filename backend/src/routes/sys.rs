use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::state::AppState;

#[derive(Deserialize, Default)]
pub struct HealthQuery {
    #[serde(default)]
    standbyok: bool,
    #[serde(default)]
    sealedok: bool,
    #[serde(default)]
    uninitok: bool,
}

/// Status codes follow Vault's `/sys/health` so load balancers and Kubernetes
/// probes can route on them:
/// 200 active · 429 unsealed standby (raft follower) · 503 sealed · 501 uninitialized.
/// `?standbyok=true` / `?sealedok=true` / `?uninitok=true` turn those into 200.
pub async fn health(State(st): State<AppState>, Query(q): Query<HealthQuery>) -> impl IntoResponse {
    let storage_ok = st.core.storage.ping().await.is_ok();
    let status = st.core.status().await.ok();
    let raft = st.core.storage.raft();
    let (standby, leader) = match raft {
        Some(r) => {
            let leader = r.peers().await.into_iter().find(|p| p["leader"] == true).map(|p| p["name"].clone());
            (!r.is_leader().await, leader)
        }
        None => (false, None),
    };
    let initialized = status.as_ref().map(|s| s.initialized).unwrap_or(false);
    let sealed = status.as_ref().map(|s| s.sealed).unwrap_or(true);

    let code = if !initialized && !q.uninitok {
        StatusCode::NOT_IMPLEMENTED
    } else if initialized && sealed && !q.sealedok {
        StatusCode::SERVICE_UNAVAILABLE
    } else if initialized && !sealed && standby && !q.standbyok {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::OK
    };
    (
        code,
        Json(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "storage": { "engine": st.core.storage.kind(), "reachable": storage_ok },
            "initialized": initialized,
            "sealed": sealed,
            "standby": standby,
            "node": raft.map(|r| r.cfg.node.name.clone()),
            "leader": leader,
        })),
    )
}
