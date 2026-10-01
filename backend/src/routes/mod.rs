mod bastion;
mod sys;

use axum::routing::get;
use axum::Router;

use crate::state::AppState;

/// The only plain-HTTP endpoints; the API itself is gRPC (`crate::grpc`).
///
/// - `GET /v1/sys/health` — Vault-style status codes for load balancers and
///   Kubernetes probes that can't speak gRPC.
/// - `GET /v1/bastion/connect` — the web-terminal WebSocket (authenticates
///   itself via the subprotocol).
/// - `GET /v1/bastion/files/download?t=` / `PUT /v1/bastion/files/upload` —
///   file transfer streams (gRPC-Web can't stream request bodies).
pub fn http_router() -> Router<AppState> {
    Router::new()
        .route("/sys/health", get(sys::health))
        .route("/bastion/connect", get(bastion::connect))
        .route("/bastion/files/download", get(bastion::download))
        .route("/bastion/files/upload", axum::routing::put(bastion::upload))
        // Not the SPA: an unknown API path is an error, not index.html.
        .fallback(|| async {
            (axum::http::StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": "not found — the API is gRPC (docs/API.md)" })))
        })
}
