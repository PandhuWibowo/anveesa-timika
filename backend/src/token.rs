//! Token auth. Tokens are random bearer strings; only their SHA-256 is used as
//! the storage key (`sys/token/<hash>`), and the entry itself is barrier-encrypted.
//! A Redis dump therefore reveals neither the token nor its policies.
//!
//! Two kinds:
//! - the **root token** (from `sys/init`) — no expiry, break-glass;
//! - **session tokens** from `auth/userpass/login` — expire after the policy's
//!   idle timeout unless renewed, and never outlive the session maximum.
//!
//! Policies (roles): `root`/`admin` = everything · `read-only` = GET only ·
//! `change-password` = may only change the (expired/initial) password.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize, Deserialize, Clone)]
pub struct TokenEntry {
    pub display_name: String,
    pub policies: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// Set for userpass sessions.
    #[serde(default)]
    pub username: Option<String>,
    /// Session end if not renewed (idle timeout). None = never (root token).
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Hard session end, renewal or not.
    #[serde(default)]
    pub max_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub idle_secs: Option<i64>,
}

impl TokenEntry {
    /// A sign-in that must first change its password or set up 2FA: it may
    /// only do that (and look itself up / sign out).
    pub fn is_restricted(&self) -> bool {
        self.policies.iter().any(|p| p == "change-password" || p == "mfa-setup")
    }

    pub fn is_admin(&self) -> bool {
        self.policies.iter().any(|p| p == "root" || p == "admin")
    }
}

pub fn storage_path(token: &str) -> String {
    format!("sys/token/{}", hex::encode(Sha256::digest(token.as_bytes())))
}

pub fn random_token() -> String {
    let mut raw = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut raw);
    format!("tmk.{}", B64URL.encode(raw))
}

/// A fresh root token: (plaintext token, storage path, serialized entry).
pub fn new_root() -> AppResult<(String, String, Vec<u8>)> {
    let token = random_token();
    let entry = TokenEntry {
        display_name: "root".into(),
        policies: vec!["root".into()],
        created_at: Utc::now(),
        username: None,
        expires_at: None,
        max_expires_at: None,
        idle_secs: None,
    };
    Ok((token.clone(), storage_path(&token), serde_json::to_vec(&entry)?))
}

/// A session token for a signed-in user.
pub fn new_session(username: &str, policies: Vec<String>, idle: Duration, max: Duration) -> (String, TokenEntry) {
    let now = Utc::now();
    let max_at = now + max;
    let entry = TokenEntry {
        display_name: username.to_string(),
        policies,
        created_at: now,
        username: Some(username.to_string()),
        expires_at: Some((now + idle).min(max_at)),
        max_expires_at: Some(max_at),
        idle_secs: Some(idle.num_seconds()),
    };
    (random_token(), entry)
}

/// Look a token up and check it's still valid: not expired, user still active.
pub async fn resolve(st: &AppState, token: &str) -> AppResult<(TokenEntry, String)> {
    let path = storage_path(token);
    let entry: TokenEntry = st
        .core
        .get_json(&path)
        .await?
        .ok_or_else(|| AppError::Auth("permission denied".into()))?;
    if entry.expires_at.is_some_and(|t| t < Utc::now()) {
        // Lazily drop the dead session; ignore races with another instance.
        let _ = st.core.commit(vec![], vec![path], vec![]).await;
        return Err(AppError::Auth("session expired — please sign in again".into()));
    }
    // A deleted or disabled user loses access immediately, not at session end.
    if let Some(u) = &entry.username {
        if !crate::auth::userpass::is_active(&st.core, u).await? {
            return Err(AppError::Auth("account is disabled or no longer exists".into()));
        }
    }
    Ok((entry, path))
}
