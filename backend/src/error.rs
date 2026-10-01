use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Missing / unknown token (401).
    #[error("{0}")]
    Auth(String),

    /// The barrier is sealed — nothing encrypted can be read or written (503).
    #[error("vault is sealed")]
    Sealed,

    /// `sys/init` has not been run yet (503).
    #[error("vault is not initialized")]
    Uninitialized,

    #[error("storage error: {0}")]
    Storage(#[from] redis::RedisError),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid request: {0}")]
    BadRequest(String),

    /// Authenticated but not allowed (403).
    #[error("{0}")]
    Forbidden(String),

    /// Too many attempts (429).
    #[error("{0}")]
    RateLimited(String),

    /// Temporarily can't serve: no Raft leader, leadership changed mid-request (503).
    #[error("{0}")]
    Unavailable(String),

    /// A commit guard failed: another instance changed the data between our
    /// read and write. Callers retry on fresh data; surfaces as 409 if they give up.
    #[error("concurrent update — please retry")]
    WriteConflict,

    /// Check-and-set mismatch or concurrent-write conflict (409).
    #[error("{0}")]
    Conflict(String),

    /// Decrypt failures, corrupt blobs, serialization bugs (500). Message is
    /// deliberately vague in the response body.
    #[error("internal error: {0}")]
    Internal(String),
}

pub type AppResult<T> = Result<T, AppError>;

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(format!("serialization: {e}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Auth(_) => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::Sealed | AppError::Uninitialized | AppError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Storage(_) => StatusCode::BAD_GATEWAY,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) | AppError::WriteConflict => StatusCode::CONFLICT,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        tracing::warn!("request failed: {self}");
        let body = match &self {
            AppError::Internal(_) => json!({ "error": "internal error" }),
            AppError::Sealed => json!({ "error": self.to_string(), "sealed": true }),
            AppError::Uninitialized => json!({ "error": self.to_string(), "initialized": false }),
            _ => json!({ "error": self.to_string() }),
        };
        let mut resp = (status, Json(body)).into_response();
        // The audit log records the full reason (the client may see less).
        resp.extensions_mut().insert(crate::audit::AuditError(self.to_string()));
        resp
    }
}
