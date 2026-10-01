//! The API: gRPC services generated from `proto/timika/v1/*.proto`.
//!
//! One port serves native gRPC (HTTP/2) and gRPC-Web (HTTP/1.1, what browsers
//! speak — via tonic-web, no proxy needed). The only non-gRPC endpoints are the
//! web-terminal WebSocket and a plain-HTTP health check for load balancers.
//!
//! Every handler starts with one of:
//!   `ctx.public()`                     — no token; still linearizes reads
//!   `ctx.who(&req, Need::Session)`     — any valid token (incl. change-password-only)
//!   `ctx.who(&req, Need::Bastion)`     — any full session (incl. the `ssh` role)
//!   `ctx.who(&req, Need::Read)`        — admin or read-only
//!   `ctx.who(&req, Need::Admin)`       — admin / root

pub mod audit;
pub mod auth;
pub mod bastion;
pub mod client;
pub mod cluster;
pub mod convert;
pub mod kv;
pub mod sys;

pub mod pb {
    tonic::include_proto!("timika.v1");
}

use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};

use crate::error::AppError;
use crate::state::AppState;
use crate::token::{self, TokenEntry};

pub type Reply<T> = Result<Response<T>, Status>;

#[derive(Clone)]
pub struct Ctx {
    pub st: AppState,
}

pub enum Need {
    /// Any valid token, even a change-password-only one.
    Session,
    /// Any full session: what an `ssh`-role user may do (their servers, their sessions).
    Bastion,
    /// Vault data and status: admin or read-only.
    Read,
    Admin,
}

impl From<AppError> for Status {
    fn from(e: AppError) -> Self {
        tracing::warn!("request failed: {e}");
        match e {
            AppError::Auth(m) => Status::unauthenticated(m),
            AppError::Forbidden(m) => Status::permission_denied(m),
            AppError::Sealed => Status::failed_precondition("vault is sealed"),
            AppError::Uninitialized => Status::failed_precondition("vault is not initialized"),
            AppError::Storage(e) => Status::unavailable(format!("storage error: {e}")),
            AppError::NotFound(m) => Status::not_found(format!("not found: {m}")),
            AppError::BadRequest(m) => Status::invalid_argument(m),
            AppError::RateLimited(m) => Status::resource_exhausted(m),
            AppError::Unavailable(m) => Status::unavailable(m),
            AppError::WriteConflict => Status::aborted("concurrent update — please retry"),
            AppError::Conflict(m) => Status::aborted(m),
            // Details stay in the server log.
            AppError::Internal(_) => Status::internal("internal error"),
        }
    }
}

pub fn token_from(md: &MetadataMap) -> Option<String> {
    md.get("x-timika-token")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .or_else(|| {
            md.get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|v| v.trim().to_string())
        })
        .filter(|t| !t.is_empty())
}

impl Ctx {
    /// Unauthenticated calls: make reads current (Raft followers) and go.
    pub async fn public(&self) -> Result<(), Status> {
        if !self.st.core.is_sealed().await {
            self.st.core.linearize().await?;
        }
        Ok(())
    }

    /// Resolve the caller's token and check the role. Returns the token entry
    /// and its storage path (for renew/revoke-self).
    pub async fn who<T>(&self, req: &Request<T>, need: Need) -> Result<(TokenEntry, String), Status> {
        let token = token_from(req.metadata())
            .ok_or_else(|| Status::unauthenticated("missing token (send `x-timika-token` metadata)"))?;
        self.st.core.linearize().await?;
        let (entry, path) = token::resolve(&self.st, &token).await?;
        let restricted = entry.is_restricted();
        let ok = match need {
            Need::Session => true,
            Need::Bastion => !restricted,
            Need::Read => !restricted && (entry.is_admin() || entry.policies.iter().any(|p| p == "read-only")),
            Need::Admin => !restricted && entry.is_admin(),
        };
        if !ok {
            return Err(Status::permission_denied(if restricted {
                if entry.policies.iter().any(|p| p == "mfa-setup") {
                "set up two-factor authentication before anything else"
            } else {
                "password change required before anything else"
            }
            } else if matches!(need, Need::Admin) {
                "administrators only"
            } else {
                "permission denied"
            }));
        }
        Ok((entry, path))
    }
}

/// Wrap a reply, tagging it with the caller for the audit log.
pub fn reply<T>(msg: T, who: &TokenEntry) -> Response<T> {
    let mut r = Response::new(msg);
    r.extensions_mut().insert(who.clone());
    r
}

/// Every gRPC service, gRPC-Web enabled, plus the standard health service.
#[allow(deprecated)] // tonic_web::enable is the simplest per-service switch in 0.12
pub fn router(st: AppState) -> axum::Router {
    use pb::audit_service_server::AuditServiceServer;
    use pb::auth_service_server::AuthServiceServer;
    use pb::bastion_service_server::BastionServiceServer;
    use pb::cluster_service_server::ClusterServiceServer;
    use pb::kv_service_server::KvServiceServer;
    use pb::sys_service_server::SysServiceServer;

    let ctx = Ctx { st: st.clone() };
    let (reporter, health) = tonic_health::server::health_reporter();
    // grpc.health.v1: SERVING only while unsealed (for gRPC-aware probes / LBs).
    tokio::spawn(async move {
        let mut reporter = reporter;
        loop {
            let status = if st.core.is_sealed().await {
                tonic_health::ServingStatus::NotServing
            } else {
                tonic_health::ServingStatus::Serving
            };
            reporter.set_service_status("", status).await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });

    tonic::service::Routes::new(tonic_web::enable(SysServiceServer::new(ctx.clone())))
        .add_service(tonic_web::enable(KvServiceServer::new(ctx.clone())))
        .add_service(tonic_web::enable(AuthServiceServer::new(ctx.clone())))
        .add_service(tonic_web::enable(BastionServiceServer::new(ctx.clone())))
        .add_service(tonic_web::enable(AuditServiceServer::new(ctx.clone())))
        .add_service(tonic_web::enable(ClusterServiceServer::new(ctx)))
        .add_service(tonic_web::enable(health))
        .into_axum_router()
}

/// Everything no route matched: gRPC calls to unknown services get a proper
/// UNIMPLEMENTED status (in their own content type); anything else is the SPA,
/// with index.html for client-side routes.
pub async fn fallback(req: axum::extract::Request) -> axum::response::Response {
    use axum::response::IntoResponse;
    use tower::ServiceExt;
    use tower_http::services::{ServeDir, ServeFile};

    let ct = req.headers().get(axum::http::header::CONTENT_TYPE).cloned();
    if ct.as_ref().and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("application/grpc")) {
        let mut r = Status::unimplemented(format!("unknown method {}", req.uri().path())).into_http().map(axum::body::Body::new);
        if let Some(ct) = ct {
            r.headers_mut().insert(axum::http::header::CONTENT_TYPE, ct);
        }
        return r.into_response();
    }
    match ServeDir::new("static").fallback(ServeFile::new("static/index.html")).oneshot(req).await {
        Ok(r) => r.map(axum::body::Body::new).into_response(),
        Err(e) => match e {},
    }
}
