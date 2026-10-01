//! Bastion (privileged access): servers ("assets"), the accounts used on
//! them, who may use them (grants), and recorded terminal sessions.
//!
//! Storage (all barrier-encrypted, like everything else):
//!   bastion/assets/<id>               server + account names (no secrets)
//!   bastion/creds/<id>/<username>     that account's password / private key
//!   bastion/grants/<id>/<grant>       who may connect to this server
//!   bastion/sessions/<sid>            session metadata
//!   bastion/rec/<sid>/<seq>           recording chunks (asciicast v2 events)
//!   bastion/cmds/<sid>/<seq>          commands run in the session (commands.rs)
//!
//! Credentials are only ever read server-side to open the SSH connection;
//! no API returns them.

pub mod archive;
pub mod commands;
pub mod files;
pub mod provision;
pub mod recorder;
pub mod session;
pub mod ssh;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::{Core, MAX_RETRIES};
use crate::error::{AppError, AppResult};
use crate::token::TokenEntry;

pub const ASSETS: &str = "bastion/assets/";
pub const SESSIONS: &str = "bastion/sessions/";

pub fn asset_path(id: &str) -> String {
    format!("{ASSETS}{id}")
}
pub fn cred_path(id: &str, user: &str) -> String {
    format!("bastion/creds/{id}/{user}")
}
pub const GRANTS: &str = "bastion/grants/";

pub fn grants_prefix(id: &str) -> String {
    format!("{GRANTS}{id}/")
}
pub fn session_path(sid: &str) -> String {
    format!("{SESSIONS}{sid}")
}
pub fn rec_prefix(sid: &str) -> String {
    format!("bastion/rec/{sid}/")
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AccountInfo {
    pub username: String,
    /// `password` or `key`
    pub auth: String,
    /// Sudo as last applied by timika (none · password · nopasswd).
    #[serde(default)]
    pub sudo: Option<String>,
    /// Extra groups timika added the account to.
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Asset {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub description: String,
    pub accounts: Vec<AccountInfo>,
    /// Pinned SSH host key fingerprint (SHA256:…), set on first connect.
    #[serde(default)]
    pub host_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Secret {
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub private_key: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Grant {
    pub id: String,
    pub asset: String,
    /// `user` or `role`
    pub subject_type: String,
    pub subject: String,
    /// Allowed account usernames; empty = all accounts on the server.
    #[serde(default)]
    pub accounts: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub id: String,
    pub user: String,
    pub asset: String,
    pub asset_name: String,
    pub host: String,
    pub account: String,
    pub client_ip: Option<String>,
    pub instance: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// connecting · live · closed · killed · failed
    pub status: String,
    pub error: Option<String>,
    pub cols: u32,
    pub rows: u32,
    pub bytes: u64,
    pub chunks: u32,
    pub truncated: bool,
    /// Commands logged so far (commands.rs).
    #[serde(default)]
    pub commands: u32,
    /// Set by an admin; the instance hosting the session ends it within ~2s.
    #[serde(default)]
    pub kill: bool,
    /// Refreshed while live; a stale one means the hosting instance died.
    pub heartbeat_at: DateTime<Utc>,
}

impl Session {
    /// `live` sessions whose instance stopped heartbeating are reported as `lost`.
    pub fn shown_status(&self) -> &str {
        if matches!(self.status.as_str(), "live" | "connecting") && (Utc::now() - self.heartbeat_at).num_seconds() > 30 {
            "lost"
        } else {
            &self.status
        }
    }
}

pub fn slug(name: &str) -> String {
    let mut s: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.trim_matches('-').chars().take(63).collect()
}

pub fn valid_account(u: &str) -> bool {
    !u.is_empty() && u.len() <= 64 && u.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
}

pub fn random_id(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    hex::encode(b)
}

pub async fn get_asset(core: &Core, id: &str) -> AppResult<Asset> {
    core.get_json(&asset_path(id)).await?.ok_or_else(|| AppError::NotFound(format!("server `{id}`")))
}

pub async fn list_assets(core: &Core) -> AppResult<Vec<Asset>> {
    let mut out = Vec::new();
    for key in core.list(ASSETS).await? {
        if let Some(a) = core.get_json::<Asset>(&key).await? {
            out.push(a);
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

pub async fn list_grants(core: &Core, asset: &str) -> AppResult<Vec<Grant>> {
    let mut out = Vec::new();
    for key in core.list(&grants_prefix(asset)).await? {
        if let Some(g) = core.get_json::<Grant>(&key).await? {
            out.push(g);
        }
    }
    Ok(out)
}

/// Which accounts on `asset` this caller may use (empty = none).
pub async fn allowed_accounts(core: &Core, me: &TokenEntry, asset: &Asset) -> AppResult<Vec<String>> {
    if me.is_admin() {
        return Ok(asset.accounts.iter().map(|a| a.username.clone()).collect());
    }
    let now = Utc::now();
    let mut allowed: Vec<String> = Vec::new();
    for g in list_grants(core, &asset.id).await? {
        if g.expires_at.is_some_and(|t| t <= now) {
            continue;
        }
        let matches = match g.subject_type.as_str() {
            "user" => me.username.as_deref() == Some(g.subject.as_str()),
            "role" => me.policies.iter().any(|p| *p == g.subject),
            _ => false,
        };
        if !matches {
            continue;
        }
        for a in &asset.accounts {
            if (g.accounts.is_empty() || g.accounts.contains(&a.username)) && !allowed.contains(&a.username) {
                allowed.push(a.username.clone());
            }
        }
    }
    Ok(allowed)
}

/// Record the host key seen on first connect (trust on first use), unless an
/// admin or another session pinned one meanwhile.
pub async fn pin_host_key(core: &Core, id: &str, fingerprint: &str) -> AppResult<()> {
    for _ in 0..MAX_RETRIES {
        let (asset, guard) = core.get_json_guarded::<Asset>(&asset_path(id)).await?;
        let Some(mut asset) = asset else { return Ok(()) };
        if asset.host_key.is_some() {
            return Ok(());
        }
        asset.host_key = Some(fingerprint.to_string());
        match core.commit(vec![(asset_path(id), serde_json::to_vec(&asset)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            other => return other,
        }
    }
    Ok(())
}

/// Delete ended sessions (and their recordings) older than the retention.
pub async fn purge_old_sessions(core: &Core, retention_days: i64) -> AppResult<usize> {
    let cutoff = Utc::now() - chrono::Duration::days(retention_days);
    let mut purged = 0;
    for key in core.list(SESSIONS).await? {
        let Some(s) = core.get_json::<Session>(&key).await? else { continue };
        let ended = s.ended_at.unwrap_or(s.heartbeat_at);
        if ended < cutoff && s.status != "live" {
            let mut deletes = core.list(&rec_prefix(&s.id)).await?;
            deletes.extend(core.list(&commands::cmd_prefix(&s.id)).await?);
            deletes.push(key);
            core.commit(vec![], deletes, vec![]).await?;
            purged += 1;
        }
    }
    Ok(purged)
}
