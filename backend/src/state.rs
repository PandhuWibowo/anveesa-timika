use std::sync::Arc;

use crate::config::Config;
use crate::core::Core;

/// Shared application state handed to every route handler.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    /// Seal state + barrier + the storage backend (Redis or Raft).
    pub core: Arc<Core>,
    /// Outbound HTTP client (captcha verification, breach checks).
    pub http: reqwest::Client,
    /// Sign-in policy, captcha, rate limiting.
    pub auth: std::sync::Arc<crate::auth::AuthService>,
    /// Open SFTP sessions for the file browser.
    pub files: std::sync::Arc<crate::bastion::files::FilePool>,
    /// Audit log (None = auditing off).
    pub audit: Option<std::sync::Arc<crate::audit::Auditor>>,
}
