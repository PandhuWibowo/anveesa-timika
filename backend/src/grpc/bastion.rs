use std::pin::Pin;
use std::sync::Arc;

use chrono::{Duration, Utc};
use serde_json::{json, Value};
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use super::convert::{ts, ts_opt};
use super::pb::bastion_service_server::BastionService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::bastion::{self, ssh, AccountInfo, Asset, Grant, Secret, Session};
use crate::core::MAX_RETRIES;
use crate::error::{AppError, AppResult};
use crate::token::TokenEntry;

fn asset_pb(a: &Asset, allowed: Vec<String>) -> pb::Asset {
    pb::Asset {
        id: a.id.clone(),
        name: a.name.clone(),
        host: a.host.clone(),
        port: a.port as u32,
        tags: a.tags.clone(),
        description: a.description.clone(),
        accounts: a
            .accounts
            .iter()
            .map(|x| pb::Account { username: x.username.clone(), auth: x.auth.clone(), sudo: x.sudo.clone(), groups: x.groups.clone() })
            .collect(),
        host_key: a.host_key.clone(),
        created_at: ts(a.created_at),
        updated_at: ts(a.updated_at),
        allowed_accounts: allowed,
    }
}

fn grant_pb(g: Grant) -> pb::Grant {
    let expired = g.expires_at.is_some_and(|t| t <= Utc::now());
    pb::Grant {
        id: g.id,
        asset: g.asset,
        subject_type: g.subject_type,
        subject: g.subject,
        accounts: g.accounts,
        expires_at: ts_opt(g.expires_at),
        created_by: g.created_by,
        created_at: ts(g.created_at),
        expired,
    }
}

fn session_pb(s: &Session) -> pb::Session {
    pb::Session {
        id: s.id.clone(),
        user: s.user.clone(),
        asset: s.asset.clone(),
        asset_name: s.asset_name.clone(),
        host: s.host.clone(),
        account: s.account.clone(),
        client_ip: s.client_ip.clone(),
        instance: s.instance.clone(),
        started_at: ts(s.started_at),
        ended_at: ts_opt(s.ended_at),
        status: s.shown_status().to_string(),
        error: s.error.clone(),
        cols: s.cols,
        rows: s.rows,
        bytes: s.bytes,
        chunks: s.chunks,
        truncated: s.truncated,
        commands: s.commands,
    }
}

fn secret_of(a: &pb::AccountInput) -> Option<Secret> {
    let has = |s: &Option<String>| s.as_deref().is_some_and(|v| !v.trim().is_empty());
    (has(&a.password) || has(&a.private_key)).then(|| Secret {
        password: a.password.clone().filter(|v| !v.is_empty()),
        private_key: a.private_key.clone().filter(|v| !v.trim().is_empty()),
        passphrase: a.passphrase.clone().filter(|v| !v.is_empty()),
    })
}

fn auth_kind(s: &Secret) -> String {
    if s.private_key.is_some() { "key".into() } else { "password".into() }
}

fn port_of(req: &pb::AssetInput) -> AppResult<u16> {
    match req.port {
        0 => Ok(22),
        p => u16::try_from(p).map_err(|_| AppError::BadRequest("port must be 1–65535".into())),
    }
}

fn clean_tags(tags: &[String]) -> Vec<String> {
    tags.iter().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect()
}

fn validate(req: &pb::AssetInput) -> AppResult<()> {
    if req.name.trim().is_empty() || bastion::slug(&req.name).is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    let host_ok = !req.host.trim().is_empty()
        && req.host.len() <= 253
        && req.host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'));
    if !host_ok {
        return Err(AppError::BadRequest("host must be a hostname or IP address".into()));
    }
    if req.accounts.is_empty() {
        return Err(AppError::BadRequest("add at least one account".into()));
    }
    for a in &req.accounts {
        if !bastion::valid_account(&a.username) {
            return Err(AppError::BadRequest(format!("invalid account name `{}`", a.username)));
        }
    }
    port_of(req)?;
    Ok(())
}

fn who_name(me: &TokenEntry) -> String {
    me.username.clone().unwrap_or_else(|| me.display_name.clone())
}

/// An account applied on the server: what to show and what to remember.
struct Provisioned {
    username: String,
    summary: String,
    sudo: Option<&'static str>,
    groups: Vec<String>,
}

/// Apply the accounts marked `provision` on the server — create, password,
/// sudo, groups — signing in with their `provision_via` account; when a
/// password was set, check it works. Pins the host key on first contact.
async fn provision_accounts(core: &crate::core::Core, asset: &mut Asset, inputs: &[pb::AccountInput]) -> AppResult<Vec<Provisioned>> {
    use bastion::provision::{Plan, Sudo};
    let mut done = Vec::new();
    for a in inputs.iter().filter(|a| a.provision) {
        let password = a.password.clone().filter(|p| !p.is_empty());
        let sudo = Sudo::parse(a.sudo.trim()).map_err(AppError::BadRequest)?;
        let groups: Vec<String> = a.groups.iter().map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect();
        if password.is_none() && sudo == Sudo::Keep && groups.is_empty() {
            return Err(AppError::BadRequest(format!("`{}`: set a password, sudo or groups to apply on the server", a.username)));
        }
        let via = a.provision_via.trim();
        if via.is_empty() {
            return Err(AppError::BadRequest(format!("`{}`: choose the account to sign in with (e.g. root)", a.username)));
        }
        // The admin account's secret: given in this form, or already stored.
        // An account managing itself signs in with its *current* password.
        let given = if via == a.username { None } else { inputs.iter().find(|x| x.username == via).and_then(secret_of) };
        let via_secret = match given {
            Some(s) => s,
            None => core.get_json::<Secret>(&bastion::cred_path(&asset.id, via)).await?.ok_or_else(|| {
                AppError::BadRequest(if via == a.username {
                    format!("`{via}` has no saved password yet to sign in with — save it first, or choose another account")
                } else {
                    format!("no credentials for `{via}` to manage `{}` with", a.username)
                })
            })?,
        };
        let plan = Plan { username: &a.username, password: password.as_deref(), sudo, groups: &groups };
        let (outcome, c) = bastion::provision::provision(asset, via, &via_secret, &plan).await.map_err(AppError::BadRequest)?;
        if asset.host_key.is_none() && !c.host_key.is_empty() {
            asset.host_key = Some(c.host_key.clone());
        }
        ssh::disconnect(&c.handle).await;
        if let Some(pw) = password {
            let secret = Secret { password: Some(pw), private_key: None, passphrase: None };
            match ssh::connect(asset, &a.username, &secret).await {
                Ok(v) => ssh::disconnect(&v.handle).await,
                Err(e) => {
                    return Err(AppError::BadRequest(format!(
                        "`{}` was {} on the server, but signing in as it failed: {e}. The server may not allow password logins for it (sshd PasswordAuthentication).",
                        a.username,
                        if outcome.created { "created" } else { "updated" }
                    )))
                }
            }
        }
        done.push(Provisioned {
            username: a.username.clone(),
            summary: outcome.summary(&a.username),
            sudo: sudo.stored(),
            groups: outcome.groups_added.clone(),
        });
    }
    Ok(done)
}

/// Remember what was applied on the server alongside each account.
fn remember(asset: &mut Asset, done: &[Provisioned]) {
    for p in done {
        if let Some(info) = asset.accounts.iter_mut().find(|x| x.username == p.username) {
            if let Some(s) = p.sudo {
                info.sudo = Some(s.to_string());
            }
            for g in &p.groups {
                if !info.groups.contains(g) {
                    info.groups.push(g.clone());
                }
            }
        }
    }
}

// ── files (SFTP) ────────────────────────────────────────────────────────────

impl Ctx {
    /// The server, after checking `me` may use `account` on it.
    async fn file_target(&self, me: &TokenEntry, asset_id: &str, account: &str) -> AppResult<Asset> {
        let asset = bastion::get_asset(&self.st.core, asset_id).await?;
        if !bastion::allowed_accounts(&self.st.core, me, &asset).await?.iter().any(|a| a == account) {
            return Err(AppError::Forbidden(format!("you don't have access to {account}@{}", asset.name)));
        }
        Ok(asset)
    }

    /// Run `op` on a pooled SFTP session; if the connection dropped, reconnect once.
    async fn with_sftp<T, F, Fut>(&self, me: &TokenEntry, asset: &Asset, account: &str, op: F) -> AppResult<T>
    where
        F: Fn(Arc<bastion::files::Conn>) -> Fut,
        Fut: std::future::Future<Output = AppResult<T>>,
    {
        let pool = &self.st.files;
        let who = who_name(me);
        let conn = pool.get(&self.st.core, asset, account, &who).await?;
        match op(conn).await {
            Err(AppError::Unavailable(_)) => {
                pool.evict(&asset.id, account, &who).await;
                op(pool.get(&self.st.core, asset, account, &who).await?).await
            }
            other => other,
        }
    }
}

fn entry_pb(name: String, m: &russh_sftp::client::fs::Metadata) -> pb::FileEntry {
    let kind = m.file_type();
    pb::FileEntry {
        name,
        kind: if kind.is_dir() { "dir" } else if kind.is_symlink() { "link" } else if kind.is_file() { "file" } else { "other" }.into(),
        size: m.size.unwrap_or(0),
        mode: bastion::files::mode_string(m.permissions.unwrap_or(0)),
        modified: m.mtime.and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)).map(ts),
        uid: m.uid.unwrap_or(0),
        gid: m.gid.unwrap_or(0),
    }
}


type ChunkStream = Pin<Box<dyn Stream<Item = Result<pb::RecordingChunk, Status>> + Send>>;

#[tonic::async_trait]
impl BastionService for Ctx {
    /// Servers this caller can use, with the accounts they may use on each.
    async fn list_assets(&self, req: Request<()>) -> Reply<pb::ListAssetsResponse> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let core = &self.st.core;
        let mut assets = Vec::new();
        for a in bastion::list_assets(core).await? {
            let allowed = bastion::allowed_accounts(core, &me, &a).await?;
            if allowed.is_empty() && !me.is_admin() {
                continue;
            }
            assets.push(asset_pb(&a, allowed));
        }
        Ok(reply(pb::ListAssetsResponse { assets, can_manage: me.is_admin() }, &me))
    }

    async fn create_asset(&self, req: Request<pb::AssetInput>) -> Reply<pb::CreateAssetResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let req = req.into_inner();
        crate::audit::target(format!("server {}", bastion::slug(&req.name)));
        validate(&req)?;
        let id = bastion::slug(&req.name);
        let now = Utc::now();
        let mut asset = Asset {
            id: id.clone(),
            name: req.name.trim().to_string(),
            host: req.host.trim().to_string(),
            port: port_of(&req)?,
            tags: clean_tags(&req.tags),
            description: req.description.trim().to_string(),
            accounts: vec![],
            host_key: None,
            created_at: now,
            updated_at: now,
        };
        let mut puts = Vec::new();
        for a in &req.accounts {
            let secret = secret_of(a)
                .ok_or_else(|| AppError::BadRequest(format!("`{}` needs a password or a private key", a.username)))?;
            asset.accounts.push(AccountInfo { username: a.username.clone(), auth: auth_kind(&secret), sudo: None, groups: vec![] });
            puts.push((bastion::cred_path(&id, &a.username), serde_json::to_vec(&secret).map_err(AppError::from)?));
        }
        let done = provision_accounts(&self.st.core, &mut asset, &req.accounts).await?;
        remember(&mut asset, &done);
        let provisioned: Vec<String> = done.iter().map(|p| p.summary.clone()).collect();
        if !provisioned.is_empty() {
            crate::audit::target(format!("server {id} · accounts {} via server login", provisioned.join(", ")));
        }
        let mut test = None;
        if req.test {
            let first = &req.accounts[0];
            let c = ssh::connect(&asset, &first.username, &secret_of(first).unwrap_or_default())
                .await
                .map_err(AppError::BadRequest)?;
            ssh::disconnect(&c.handle).await;
            asset.host_key = Some(c.host_key.clone());
            test = Some(pb::TestResult {
                ok: true,
                account: first.username.clone(),
                host_key: Some(c.host_key),
                connect_ms: c.connect_ms as u64,
                error: None,
            });
        }
        puts.push((bastion::asset_path(&id), serde_json::to_vec(&asset).map_err(AppError::from)?));
        let guard = crate::storage::Guard { path: bastion::asset_path(&id), expect: None };
        match self.st.core.commit(puts, vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => Err(Status::already_exists(format!("a server named `{id}` already exists"))),
            Err(e) => Err(e.into()),
            Ok(()) => {
                let allowed = asset.accounts.iter().map(|a| a.username.clone()).collect();
                Ok(reply(pb::CreateAssetResponse { asset: Some(asset_pb(&asset, allowed)), test, provisioned }, &me))
            }
        }
    }

    async fn update_asset(&self, req: Request<pb::UpdateAssetRequest>) -> Reply<pb::Asset> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let pb::UpdateAssetRequest { id, asset: input } = req.into_inner();
        crate::audit::target(format!("server {id}"));
        let input = input.ok_or_else(|| Status::invalid_argument("asset is required"))?;
        validate(&input)?;
        let core = &self.st.core;
        for _ in 0..MAX_RETRIES {
            let (cur, guard) = core.get_json_guarded::<Asset>(&bastion::asset_path(&id)).await?;
            let mut asset = cur.ok_or_else(|| AppError::NotFound(format!("server `{id}`")))?;
            let port = port_of(&input)?;
            if asset.host != input.host.trim() || asset.port != port {
                asset.host_key = None; // a different machine: pin its key afresh
            }
            asset.name = input.name.trim().to_string();
            asset.host = input.host.trim().to_string();
            asset.port = port;
            asset.tags = clean_tags(&input.tags);
            asset.description = input.description.trim().to_string();
            asset.updated_at = Utc::now();
            let done = provision_accounts(core, &mut asset, &input.accounts).await?;
            if !done.is_empty() {
                crate::audit::target(format!(
                    "server {id} · accounts {} via server login",
                    done.iter().map(|p| p.summary.as_str()).collect::<Vec<_>>().join(", ")
                ));
            }

            let mut puts = Vec::new();
            let mut accounts = Vec::new();
            for a in &input.accounts {
                let existing = asset.accounts.iter().find(|x| x.username == a.username).cloned();
                match (secret_of(a), existing) {
                    (Some(secret), prev) => {
                        let (sudo, groups) = prev.map(|e| (e.sudo, e.groups)).unwrap_or_default();
                        accounts.push(AccountInfo { username: a.username.clone(), auth: auth_kind(&secret), sudo, groups });
                        puts.push((bastion::cred_path(&id, &a.username), serde_json::to_vec(&secret).map_err(AppError::from)?));
                    }
                    // No new secret: keep the stored one.
                    (None, Some(info)) => accounts.push(info),
                    (None, None) => {
                        return Err(Status::invalid_argument(format!("`{}` needs a password or a private key", a.username)))
                    }
                }
            }
            let deletes: Vec<String> = asset
                .accounts
                .iter()
                .filter(|old| !accounts.iter().any(|n| n.username == old.username))
                .map(|old| bastion::cred_path(&id, &old.username))
                .collect();
            asset.accounts = accounts;
            remember(&mut asset, &done);
            puts.push((bastion::asset_path(&id), serde_json::to_vec(&asset).map_err(AppError::from)?));
            match core.commit(puts, deletes, vec![guard]).await {
                Err(AppError::WriteConflict) => continue,
                Err(e) => return Err(e.into()),
                Ok(()) => {
                    let allowed = asset.accounts.iter().map(|a| a.username.clone()).collect();
                    return Ok(reply(asset_pb(&asset, allowed), &me));
                }
            }
        }
        Err(AppError::WriteConflict.into())
    }

    async fn delete_asset(&self, req: Request<pb::AssetRef>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = &req.get_ref().id;
        crate::audit::target(format!("server {id}"));
        let core = &self.st.core;
        let asset = bastion::get_asset(core, id).await?;
        let mut deletes: Vec<String> = asset.accounts.iter().map(|a| bastion::cred_path(id, &a.username)).collect();
        deletes.extend(core.list(&bastion::grants_prefix(id)).await?);
        deletes.push(bastion::asset_path(id));
        core.commit(vec![], deletes, vec![]).await?;
        Ok(reply((), &me))
    }

    async fn test_asset(&self, req: Request<pb::TestAssetRequest>) -> Reply<pb::TestResult> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        crate::audit::target(format!("server {}", r.id));
        let core = &self.st.core;
        let asset = bastion::get_asset(core, &r.id).await?;
        let account = r
            .account
            .filter(|a| !a.is_empty())
            .or_else(|| asset.accounts.first().map(|a| a.username.clone()))
            .ok_or_else(|| Status::invalid_argument("server has no accounts"))?;
        let secret: Secret = core.get_json(&bastion::cred_path(&r.id, &account)).await?.unwrap_or_default();
        let result = match ssh::connect(&asset, &account, &secret).await {
            Ok(c) => {
                ssh::disconnect(&c.handle).await;
                if c.first_seen {
                    bastion::pin_host_key(core, &r.id, &c.host_key).await?;
                }
                pb::TestResult { ok: true, account, host_key: Some(c.host_key), connect_ms: c.connect_ms as u64, error: None }
            }
            Err(message) => pb::TestResult { ok: false, account, host_key: None, connect_ms: 0, error: Some(message) },
        };
        Ok(reply(result, &me))
    }

    async fn reset_host_key(&self, req: Request<pb::AssetRef>) -> Reply<pb::Asset> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = &req.get_ref().id;
        crate::audit::target(format!("server {id} (host key reset)"));
        let core = &self.st.core;
        for _ in 0..MAX_RETRIES {
            let (cur, guard) = core.get_json_guarded::<Asset>(&bastion::asset_path(id)).await?;
            let mut asset = cur.ok_or_else(|| AppError::NotFound(format!("server `{id}`")))?;
            asset.host_key = None;
            let blob = serde_json::to_vec(&asset).map_err(AppError::from)?;
            match core.commit(vec![(bastion::asset_path(id), blob)], vec![], vec![guard]).await {
                Err(AppError::WriteConflict) => continue,
                Err(e) => return Err(e.into()),
                Ok(()) => {
                    let allowed = asset.accounts.iter().map(|a| a.username.clone()).collect();
                    return Ok(reply(asset_pb(&asset, allowed), &me));
                }
            }
        }
        Err(AppError::WriteConflict.into())
    }

    async fn list_grants(&self, req: Request<pb::AssetRef>) -> Reply<pb::ListGrantsResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let grants = bastion::list_grants(&self.st.core, &req.get_ref().id).await?;
        Ok(reply(pb::ListGrantsResponse { grants: grants.into_iter().map(grant_pb).collect() }, &me))
    }

    async fn list_all_grants(&self, req: Request<()>) -> Reply<pb::ListGrantsResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let mut grants = Vec::new();
        for key in core.list(bastion::GRANTS).await? {
            if let Some(g) = core.get_json::<Grant>(&key).await? {
                grants.push(grant_pb(g));
            }
        }
        Ok(reply(pb::ListGrantsResponse { grants }, &me))
    }

    async fn create_grant(&self, req: Request<pb::CreateGrantRequest>) -> Reply<pb::Grant> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        crate::audit::target(format!("{} {} → server {}{}", r.subject_type, r.subject.trim(), r.asset, if r.accounts.is_empty() { String::new() } else { format!(" ({})", r.accounts.join(", ")) }));
        let core = &self.st.core;
        let asset = bastion::get_asset(core, &r.asset).await?;
        if !matches!(r.subject_type.as_str(), "user" | "role") {
            return Err(Status::invalid_argument("subject_type must be `user` or `role`"));
        }
        let subject = r.subject.trim().to_lowercase();
        if subject.is_empty() {
            return Err(Status::invalid_argument("subject is required"));
        }
        if r.subject_type == "user" && core.get_json::<Value>(&format!("auth/userpass/users/{subject}")).await?.is_none() {
            return Err(Status::invalid_argument(format!("no user named `{subject}`")));
        }
        for a in &r.accounts {
            if !asset.accounts.iter().any(|x| &x.username == a) {
                return Err(Status::invalid_argument(format!("`{}` has no account `{a}`", asset.name)));
            }
        }
        let g = Grant {
            id: bastion::random_id(6),
            asset: asset.id.clone(),
            subject_type: r.subject_type,
            subject,
            accounts: r.accounts,
            expires_at: r.hours.filter(|h| *h > 0).map(|h| Utc::now() + Duration::hours(h)),
            created_by: who_name(&me),
            created_at: Utc::now(),
        };
        let blob = serde_json::to_vec(&g).map_err(AppError::from)?;
        core.commit(vec![(format!("{}{}", bastion::grants_prefix(&asset.id), g.id), blob)], vec![], vec![]).await?;
        Ok(reply(grant_pb(g), &me))
    }

    async fn delete_grant(&self, req: Request<pb::DeleteGrantRequest>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        crate::audit::target(format!("grant {} on server {}", r.grant_id, r.asset));
        self.st.core.commit(vec![], vec![format!("{}{}", bastion::grants_prefix(&r.asset), r.grant_id)], vec![]).await?;
        Ok(reply((), &me))
    }

    /// Admins see every session; others see their own.
    async fn list_sessions(&self, req: Request<()>) -> Reply<pb::ListSessionsResponse> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let who = who_name(&me);
        let core = &self.st.core;
        let mut all: Vec<Session> = Vec::new();
        for key in core.list(bastion::SESSIONS).await? {
            if let Some(s) = core.get_json::<Session>(&key).await? {
                if me.is_admin() || s.user == who {
                    all.push(s);
                }
            }
        }
        all.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        all.truncate(300);
        Ok(reply(pb::ListSessionsResponse { sessions: all.iter().map(session_pb).collect() }, &me))
    }

    async fn kill_session(&self, req: Request<pb::SessionRef>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let sid = &req.get_ref().id;
        crate::audit::target(format!("session {sid}"));
        let core = &self.st.core;
        for _ in 0..MAX_RETRIES {
            let (cur, guard) = core.get_json_guarded::<Session>(&bastion::session_path(sid)).await?;
            let mut s = cur.ok_or_else(|| AppError::NotFound(format!("session `{sid}`")))?;
            s.kill = true;
            let blob = serde_json::to_vec(&s).map_err(AppError::from)?;
            match core.commit(vec![(bastion::session_path(sid), blob)], vec![], vec![guard]).await {
                Err(AppError::WriteConflict) => continue,
                Err(e) => return Err(e.into()),
                Ok(()) => return Ok(reply((), &me)),
            }
        }
        Err(AppError::WriteConflict.into())
    }

    async fn list_commands(&self, req: Request<pb::ListCommandsRequest>) -> Reply<pb::ListCommandsResponse> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        if !r.asset.is_empty() {
            crate::audit::target(format!("server {}", r.asset));
        }
        let core = &self.st.core;
        let who = who_name(&me);
        let limit = match r.limit { 0 => 200, n => n.min(1000) } as usize;
        let q = r.query.trim().to_lowercase();

        let mut sessions: Vec<Session> = Vec::new();
        if !r.session.is_empty() {
            let s: Session = core
                .get_json(&bastion::session_path(&r.session))
                .await?
                .ok_or_else(|| AppError::NotFound(format!("session `{}`", r.session)))?;
            if !me.is_admin() && s.user != who {
                return Err(Status::permission_denied("not your session"));
            }
            sessions.push(s);
        } else {
            for key in core.list(bastion::SESSIONS).await? {
                if let Some(s) = core.get_json::<Session>(&key).await? {
                    let mine = me.is_admin() || s.user == who;
                    let asset_ok = r.asset.is_empty() || s.asset == r.asset;
                    let user_ok = r.user.is_empty() || s.user.eq_ignore_ascii_case(r.user.trim());
                    if mine && asset_ok && user_ok && s.commands > 0 {
                        sessions.push(s);
                    }
                }
            }
            sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        }

        let mut out = Vec::new();
        let mut truncated = false;
        'sessions: for s in &sessions {
            let mut chunks = core.list(&bastion::commands::cmd_prefix(&s.id)).await?;
            chunks.sort();
            for key in chunks.iter().rev() {
                let list: Vec<serde_json::Value> = core.get_json(key).await?.unwrap_or_default();
                for c in list.iter().rev() {
                    let command = c["command"].as_str().unwrap_or_default();
                    let risk = c["risk"].as_str().map(String::from);
                    if (!q.is_empty() && !command.to_lowercase().contains(&q)) || (r.risky_only && risk.is_none()) {
                        continue;
                    }
                    if out.len() >= limit {
                        truncated = true;
                        break 'sessions;
                    }
                    out.push(pb::Command {
                        session: s.id.clone(),
                        time: c["time"].as_str().unwrap_or_default().to_string(),
                        offset: c["t"].as_f64().unwrap_or(0.0),
                        command: command.to_string(),
                        source: c["source"].as_str().unwrap_or("typed").to_string(),
                        risk,
                        user: s.user.clone(),
                        asset: s.asset.clone(),
                        asset_name: s.asset_name.clone(),
                        account: s.account.clone(),
                    });
                }
            }
        }
        Ok(reply(pb::ListCommandsResponse { commands: out, truncated }, &me))
    }

    async fn list_files(&self, req: Request<pb::FilePath>) -> Reply<pb::ListFilesResponse> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        let asset = self.file_target(&me, &r.asset, &r.account).await?;
        let path = bastion::files::clean_remote(&r.path)?;
        let (dir, entries) = self
            .with_sftp(&me, &asset, &r.account, |c| {
                let path = path.clone();
                async move {
                    let dir = c.sftp.canonicalize(if path.is_empty() { ".".to_string() } else { path.clone() }).await.map_err(|e| bastion::files::sftp_error(&path, e))?;
                    let mut entries: Vec<pb::FileEntry> = c
                        .sftp
                        .read_dir(dir.clone())
                        .await
                        .map_err(|e| bastion::files::sftp_error(&dir, e))?
                        .filter(|e| e.file_name() != "." && e.file_name() != "..")
                        .map(|e| entry_pb(e.file_name(), &e.metadata()))
                        .collect();
                    entries.sort_by(|a, b| (b.kind == "dir").cmp(&(a.kind == "dir")).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                    Ok((dir, entries))
                }
            })
            .await?;
        crate::audit::target(format!("{}@{}:{dir}", r.account, r.asset));
        Ok(reply(pb::ListFilesResponse { path: dir, entries }, &me))
    }

    async fn make_dir(&self, req: Request<pb::FilePath>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        crate::audit::target(format!("{}@{}:{} (new folder)", r.account, r.asset, r.path));
        let asset = self.file_target(&me, &r.asset, &r.account).await?;
        let path = bastion::files::clean_remote(&r.path)?;
        self.with_sftp(&me, &asset, &r.account, |c| {
            let path = path.clone();
            async move { c.sftp.create_dir(path.clone()).await.map_err(|e| bastion::files::sftp_error(&path, e)) }
        })
        .await?;
        Ok(reply((), &me))
    }

    async fn rename_file(&self, req: Request<pb::RenameFileRequest>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        crate::audit::target(format!("{}@{}:{} → {}", r.account, r.asset, r.from, r.to));
        let asset = self.file_target(&me, &r.asset, &r.account).await?;
        let (from, to) = (bastion::files::clean_remote(&r.from)?, bastion::files::clean_remote(&r.to)?);
        self.with_sftp(&me, &asset, &r.account, |c| {
            let (from, to) = (from.clone(), to.clone());
            async move {
                if c.sftp.try_exists(to.clone()).await.unwrap_or(false) {
                    return Err(AppError::Conflict(format!("`{to}` already exists")));
                }
                c.sftp.rename(from.clone(), to).await.map_err(|e| bastion::files::sftp_error(&from, e))
            }
        })
        .await?;
        Ok(reply((), &me))
    }

    async fn delete_files(&self, req: Request<pb::DeleteFilesRequest>) -> Reply<pb::DeleteFilesResponse> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        crate::audit::target(format!("{}@{}:{} (delete)", r.account, r.asset, r.paths.join(", ")));
        let asset = self.file_target(&me, &r.asset, &r.account).await?;
        for p in &r.paths {
            bastion::files::clean_remote(p)?;
            if p.trim_end_matches('/').is_empty() {
                return Err(Status::invalid_argument("refusing to delete /"));
            }
        }
        let deleted = self
            .with_sftp(&me, &asset, &r.account, |c| {
                let paths = r.paths.clone();
                async move {
                    let mut n = 0u32;
                    for root in paths {
                        // Depth-first: (path, children listed yet?)
                        let mut stack = vec![(root, false)];
                        while let Some((path, listed)) = stack.pop() {
                            let meta = c.sftp.symlink_metadata(path.clone()).await.map_err(|e| bastion::files::sftp_error(&path, e))?;
                            if meta.file_type().is_dir() {
                                if listed {
                                    c.sftp.remove_dir(path.clone()).await.map_err(|e| bastion::files::sftp_error(&path, e))?;
                                    n += 1;
                                } else {
                                    stack.push((path.clone(), true));
                                    for e in c.sftp.read_dir(path.clone()).await.map_err(|e| bastion::files::sftp_error(&path, e))? {
                                        let name = e.file_name();
                                        if name != "." && name != ".." {
                                            stack.push((bastion::files::join(&path, &name), false));
                                        }
                                    }
                                    if stack.len() > 50_000 {
                                        return Err(AppError::BadRequest("too many files to delete at once".into()));
                                    }
                                }
                            } else {
                                c.sftp.remove_file(path.clone()).await.map_err(|e| bastion::files::sftp_error(&path, e))?;
                                n += 1;
                            }
                        }
                    }
                    Ok(n)
                }
            })
            .await?;
        Ok(reply(pb::DeleteFilesResponse { deleted }, &me))
    }

    async fn download_link(&self, req: Request<pb::FilePath>) -> Reply<pb::DownloadLinkResponse> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        let asset = self.file_target(&me, &r.asset, &r.account).await?;
        let path = bastion::files::clean_remote(&r.path)?;
        let meta = self
            .with_sftp(&me, &asset, &r.account, |c| {
                let path = path.clone();
                async move { c.sftp.metadata(path.clone()).await.map_err(|e| bastion::files::sftp_error(&path, e)) }
            })
            .await?;
        if meta.file_type().is_dir() {
            return Err(Status::invalid_argument("folders can't be downloaded — open it and download files"));
        }
        let key = self.st.core.derive_key(b"timika files ticket v1").await.ok_or(AppError::Sealed)?;
        let ticket = bastion::files::Ticket {
            asset: asset.id.clone(),
            account: r.account.clone(),
            path: path.clone(),
            who: who_name(&me),
            username: me.username.clone(),
            policies: me.policies.clone(),
            exp: Utc::now().timestamp() + bastion::files::TICKET_TTL_SECS,
            names: vec![],
            format: None,
        };
        let name = path.rsplit('/').next().unwrap_or("download").to_string();
        Ok(reply(
            pb::DownloadLinkResponse { url: format!("/v1/bastion/files/download?t={}", bastion::files::sign(&key, &ticket)), name, size: meta.size.unwrap_or(0) },
            &me,
        ))
    }

    async fn compress(&self, req: Request<pb::CompressRequest>) -> Reply<pb::CompressResponse> {
        use bastion::archive::{self, Format};
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        let format = Format::parse(&r.format)?;
        let mut name = r.archive.trim().to_string();
        if name.is_empty() {
            name = if r.names.len() == 1 { r.names[0].clone() } else { "archive".into() };
        }
        if !name.to_lowercase().ends_with(format.ext()) {
            name.push_str(format.ext());
        }
        crate::audit::target(format!("{}@{}:{} (compress {} → {name})", r.account, r.asset, r.dir, r.names.join(", ")));
        let asset = self.file_target(&me, &r.asset, &r.account).await?;
        let dir = bastion::files::clean_remote(&r.dir)?;
        let cmd = archive::compress_cmd(&dir, &r.names, format, &name)?;
        let path = bastion::files::join(&dir, &name);
        let size = self
            .with_sftp(&me, &asset, &r.account, |c| {
                let (cmd, path) = (cmd.clone(), path.clone());
                async move {
                    let (status, out) = ssh::exec_for(&c.handle, &cmd, b"", std::time::Duration::from_secs(1800))
                        .await
                        .map_err(AppError::Unavailable)?;
                    if status != 0 {
                        return Err(archive::explain(status, &out, "compressing"));
                    }
                    Ok(c.sftp.metadata(path.clone()).await.map_err(|e| bastion::files::sftp_error(&path, e))?.size.unwrap_or(0))
                }
            })
            .await?;
        Ok(reply(pb::CompressResponse { path, size }, &me))
    }

    async fn extract(&self, req: Request<pb::FilePath>) -> Reply<pb::ExtractResponse> {
        use bastion::archive;
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        crate::audit::target(format!("{}@{}:{} (extract)", r.account, r.asset, r.path));
        let asset = self.file_target(&me, &r.asset, &r.account).await?;
        let path = bastion::files::clean_remote(&r.path)?;
        let (dir, file) = path.rsplit_once('/').map(|(d, f)| (if d.is_empty() { "/" } else { d }, f)).ok_or_else(|| Status::invalid_argument("give the archive's full path"))?;
        let (_, stem) = archive::archive_kind(file).ok_or_else(|| Status::invalid_argument(format!("`{file}` isn't a zip or tar archive")))?;
        let (dir, file) = (dir.to_string(), file.to_string());
        let into = self
            .with_sftp(&me, &asset, &r.account, |c| {
                let (dir, file, stem) = (dir.clone(), file.clone(), stem.clone());
                async move {
                    // site.tar.gz → site/, or site (2)/ … if that exists.
                    let mut into = stem.clone();
                    for n in 2..100 {
                        if !c.sftp.try_exists(bastion::files::join(&dir, &into)).await.unwrap_or(false) {
                            break;
                        }
                        into = format!("{stem} ({n})");
                    }
                    let cmd = archive::extract_cmd(&dir, &file, &into)?;
                    let (status, out) = ssh::exec_for(&c.handle, &cmd, b"", std::time::Duration::from_secs(1800))
                        .await
                        .map_err(AppError::Unavailable)?;
                    if status != 0 {
                        return Err(archive::explain(status, &out, "extracting"));
                    }
                    Ok(bastion::files::join(&dir, &into))
                }
            })
            .await?;
        Ok(reply(pb::ExtractResponse { dir: into }, &me))
    }

    async fn archive_link(&self, req: Request<pb::ArchiveLinkRequest>) -> Reply<pb::DownloadLinkResponse> {
        use bastion::archive::{self, Format};
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let r = req.into_inner();
        let format = Format::parse(&r.format)?;
        let asset = self.file_target(&me, &r.asset, &r.account).await?;
        let dir = bastion::files::clean_remote(&r.dir)?;
        archive::stream_cmd(&dir, &r.names, format)?; // validates the names
        let key = self.st.core.derive_key(b"timika files ticket v1").await.ok_or(AppError::Sealed)?;
        let base = if r.names.len() == 1 { r.names[0].clone() } else { dir.rsplit('/').find(|s| !s.is_empty()).unwrap_or("files").to_string() };
        let ticket = bastion::files::Ticket {
            asset: asset.id.clone(),
            account: r.account.clone(),
            path: dir,
            who: who_name(&me),
            username: me.username.clone(),
            policies: me.policies.clone(),
            exp: Utc::now().timestamp() + bastion::files::TICKET_TTL_SECS,
            names: r.names.clone(),
            format: Some(r.format.clone()),
        };
        Ok(reply(
            pb::DownloadLinkResponse { url: format!("/v1/bastion/files/download?t={}", bastion::files::sign(&key, &ticket)), name: format!("{base}{}", format.ext()), size: 0 },
            &me,
        ))
    }

    type GetRecordingStream = ChunkStream;

    /// asciicast v2: a header line, then the stored event chunks in order —
    /// streamed, so long sessions never sit in memory whole.
    async fn get_recording(&self, req: Request<pb::SessionRef>) -> Reply<Self::GetRecordingStream> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let sid = req.get_ref().id.clone();
        crate::audit::target(format!("recording {sid}"));
        let core = self.st.core.clone();
        let s: Session = core
            .get_json(&bastion::session_path(&sid))
            .await?
            .ok_or_else(|| AppError::NotFound(format!("session `{sid}`")))?;
        if !me.is_admin() && s.user != who_name(&me) {
            return Err(Status::permission_denied("not your session"));
        }
        let header = json!({
            "version": 2, "width": s.cols.max(20), "height": s.rows.max(5),
            "timestamp": s.started_at.timestamp(),
            "title": format!("{}@{} — {}", s.account, s.asset_name, s.user),
            "env": { "TERM": "xterm-256color" },
        });
        let keys = core.list(&bastion::rec_prefix(&sid)).await?;
        let stream = async_stream(header.to_string() + "\n", keys, core);
        Ok(reply(Box::pin(stream) as ChunkStream, &me))
    }
}

fn async_stream(
    header: String,
    keys: Vec<String>,
    core: std::sync::Arc<crate::core::Core>,
) -> impl Stream<Item = Result<pb::RecordingChunk, Status>> + Send {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        if tx.send(Ok(pb::RecordingChunk { data: header.into_bytes() })).await.is_err() {
            return;
        }
        for key in keys {
            let item = match core.get(&key).await {
                Ok(Some(data)) => Ok(pb::RecordingChunk { data }),
                Ok(None) => continue,
                Err(e) => Err(Status::from(e)),
            };
            let stop = item.is_err();
            if tx.send(item).await.is_err() || stop {
                return;
            }
        }
    });
    tokio_stream::wrappers::ReceiverStream::new(rx)
}

// Keep Response in scope for the trait's generated signatures.
#[allow(dead_code)]
type _R = Response<()>;
