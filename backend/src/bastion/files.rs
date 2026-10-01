//! File transfer (SFTP) through the bastion: browse, upload, download, rename,
//! delete — with the server's stored credentials and pinned host key, so files
//! never need a key on the user's laptop either.
//!
//! One SFTP session per (person, server, account) is kept open and reused while
//! they browse, then closed after `IDLE` without use. Downloads use a short-lived
//! signed link (the browser streams straight to disk); uploads stream through.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use russh_sftp::client::SftpSession;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::Mutex;

use super::ssh::{self, Pinned};
use super::{cred_path, pin_host_key, Asset, Secret};
use crate::core::Core;
use crate::error::{AppError, AppResult};

const IDLE: Duration = Duration::from_secs(120);
pub const TICKET_TTL_SECS: i64 = 60;

pub struct Conn {
    pub sftp: SftpSession,
    /// The SSH connection under the SFTP session (also runs tar/zip).
    pub handle: russh::client::Handle<Pinned>,
    last: std::sync::Mutex<Instant>,
}

impl Conn {
    fn touch(&self) {
        *self.last.lock().unwrap() = Instant::now();
    }
}

#[derive(Default)]
pub struct FilePool {
    conns: Mutex<HashMap<String, Arc<Conn>>>,
}

impl FilePool {
    pub fn new() -> Arc<Self> {
        let pool = Arc::new(Self::default());
        let weak = Arc::downgrade(&pool);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let Some(pool) = weak.upgrade() else { break };
                let idle: Vec<Arc<Conn>> = {
                    let mut m = pool.conns.lock().await;
                    let keys: Vec<String> = m.iter().filter(|(_, c)| c.last.lock().unwrap().elapsed() > IDLE).map(|(k, _)| k.clone()).collect();
                    keys.iter().filter_map(|k| m.remove(k)).collect()
                };
                for c in idle {
                    let _ = c.sftp.close().await;
                    ssh::disconnect(&c.handle).await;
                }
            }
        });
        pool
    }

    fn key(user: &str, asset: &str, account: &str) -> String {
        format!("{user}\u{0}{asset}\u{0}{account}")
    }

    /// A live SFTP session for `user` on `asset` as `account` (reused or new).
    pub async fn get(&self, core: &Core, asset: &Asset, account: &str, user: &str) -> AppResult<Arc<Conn>> {
        let key = Self::key(user, &asset.id, account);
        if let Some(c) = self.conns.lock().await.get(&key).cloned() {
            if !c.handle.is_closed() {
                c.touch();
                return Ok(c);
            }
        }
        let conn = Arc::new(open(core, asset, account).await?);
        self.conns.lock().await.insert(key, conn.clone());
        Ok(conn)
    }

    /// Drop a session that failed (it is reopened on the next request).
    pub async fn evict(&self, asset: &str, account: &str, user: &str) {
        if let Some(c) = self.conns.lock().await.remove(&Self::key(user, asset, account)) {
            ssh::disconnect(&c.handle).await;
        }
    }
}

/// A new SSH + SFTP connection as `account` on `asset` (stored credentials,
/// pinned host key). The pool reuses these; automation runs open their own.
pub async fn open(core: &Core, asset: &Asset, account: &str) -> AppResult<Conn> {
    let secret: Secret = core
        .get_json(&cred_path(&asset.id, account))
        .await?
        .ok_or_else(|| AppError::NotFound("account credentials".into()))?;
    let c = ssh::connect(asset, account, &secret).await.map_err(AppError::BadRequest)?;
    if c.first_seen && !c.host_key.is_empty() {
        let _ = pin_host_key(core, &asset.id, &c.host_key).await;
    }
    let ch = c.handle.channel_open_session().await.map_err(|e| AppError::BadRequest(format!("could not open a session: {e}")))?;
    ch.request_subsystem(true, "sftp").await.map_err(|e| AppError::BadRequest(format!("the server has no SFTP: {e}")))?;
    let sftp = SftpSession::new(ch.into_stream())
        .await
        .map_err(|e| AppError::BadRequest(format!("SFTP is not available on this server: {e}")))?;
    sftp.set_timeout(30);
    Ok(Conn { sftp, handle: c.handle, last: std::sync::Mutex::new(Instant::now()) })
}

/// An SFTP error as something to tell the user.
pub fn sftp_error(path: &str, e: russh_sftp::client::error::Error) -> AppError {
    use russh_sftp::client::error::Error as E;
    use russh_sftp::protocol::StatusCode as S;
    match e {
        E::Status(s) => match s.status_code {
            S::NoSuchFile => AppError::NotFound(format!("`{path}`")),
            S::PermissionDenied => AppError::Forbidden(format!("permission denied: `{path}`")),
            _ => AppError::BadRequest(format!("`{path}`: {}", if s.error_message.is_empty() { format!("{:?}", s.status_code) } else { s.error_message })),
        },
        other => AppError::Unavailable(format!("file transfer failed: {other}")),
    }
}

/// `drwxr-xr-x` from a mode.
pub fn mode_string(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        _ => '-',
    };
    let mut s = String::from(kind);
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// Absolute, normalized remote path ("" = home is resolved by the server).
pub fn clean_remote(path: &str) -> AppResult<String> {
    if path.contains('\0') || path.len() > 4096 {
        return Err(AppError::BadRequest("invalid path".into()));
    }
    Ok(path.to_string())
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

// ── download links ──────────────────────────────────────────────────────────

/// What a download link authorizes: one file, for one person, for a minute.
#[derive(Serialize, Deserialize)]
pub struct Ticket {
    pub asset: String,
    pub account: String,
    pub path: String,
    /// The person's display name / username and roles (for the audit log).
    pub who: String,
    pub username: Option<String>,
    pub policies: Vec<String>,
    pub exp: i64,
    /// Set for "download as archive": `path` is the folder, these the names in it.
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub format: Option<String>,
}

fn mac(key: &[u8; 32], body: &str) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    m.update(body.as_bytes());
    B64URL.encode(m.finalize().into_bytes())
}

pub fn sign(key: &[u8; 32], t: &Ticket) -> String {
    let body = B64URL.encode(serde_json::to_vec(t).unwrap_or_default());
    format!("{body}.{}", mac(key, &body))
}

pub fn verify(key: &[u8; 32], ticket: &str, now: DateTime<Utc>) -> AppResult<Ticket> {
    let bad = || AppError::Auth("this download link is invalid or has expired".into());
    let (body, sig) = ticket.split_once('.').ok_or_else(bad)?;
    // Constant-time comparison (hmac's verify).
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    m.update(body.as_bytes());
    m.verify_slice(&B64URL.decode(sig).map_err(|_| bad())?).map_err(|_| bad())?;
    let t: Ticket = serde_json::from_slice(&B64URL.decode(body).map_err(|_| bad())?).map_err(|_| bad())?;
    if t.exp < now.timestamp() {
        return Err(bad());
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(mode_string(0o040755), "drwxr-xr-x");
        assert_eq!(mode_string(0o100640), "-rw-r-----");
        assert_eq!(mode_string(0o120777), "lrwxrwxrwx");
    }

    #[test]
    fn tickets() {
        let key = [7u8; 32];
        let t = Ticket { asset: "web".into(), account: "deploy".into(), path: "/etc/hosts".into(), who: "jane".into(), username: Some("jane".into()), policies: vec![], exp: Utc::now().timestamp() + 60, names: vec![], format: None };
        let s = sign(&key, &t);
        assert_eq!(verify(&key, &s, Utc::now()).unwrap().path, "/etc/hosts");
        // Tampered path, wrong key, expired.
        let (body, sig) = s.split_once('.').unwrap();
        let mut forged: Ticket = serde_json::from_slice(&B64URL.decode(body).unwrap()).unwrap();
        forged.path = "/etc/shadow".into();
        let forged = format!("{}.{sig}", B64URL.encode(serde_json::to_vec(&forged).unwrap()));
        assert!(verify(&key, &forged, Utc::now()).is_err());
        assert!(verify(&[8u8; 32], &s, Utc::now()).is_err());
        assert!(verify(&key, &s, Utc::now() + chrono::Duration::seconds(120)).is_err());
    }
}
