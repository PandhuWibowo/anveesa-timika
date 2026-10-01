//! Vault core: init → unseal → (serve) → seal, plus Raft cluster join.
//!
//! The only plaintext record in storage is `core/seal-config` (share counts, no
//! secrets). Everything else goes through the barrier. The root key is never
//! persisted: it exists only as Shamir shares held by operators, and in memory
//! while the vault is unsealed. A process restart therefore always comes back
//! **sealed** — the data in storage survives, but is unreadable until enough
//! operators submit their shares again.
//!
//! With Raft storage each node is sealed independently, and a node only takes
//! part in Raft while unsealed (the cluster key is derived from the root key).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sharks::{Share, Sharks};
use tokio::sync::{Mutex, RwLock};
use zeroize::Zeroizing;

use crate::barrier::{self, Barrier, Key, KeyringDoc, ROOT_TERM};
use crate::error::{AppError, AppResult};
use crate::raft::cluster_auth;
use crate::raft::types::ClusterNode;
use crate::seal::{AutoSeal, WrappedKey};
use crate::storage::{Guard, Storage};
use crate::token;

pub const SEAL_CONFIG_PATH: &str = "core/seal-config";
pub const KEYRING_PATH: &str = "core/keyring";
/// Auto-unseal only: the root key wrapped by the key service (see `seal`).
pub const WRAPPED_ROOT_PATH: &str = "core/root-key-wrapped";
const CHALLENGE_TTL: Duration = Duration::from_secs(300);
/// Attempts for an optimistic read-modify-write before giving up with 409.
pub const MAX_RETRIES: usize = 8;

#[derive(Serialize, Deserialize, Clone)]
pub struct SealConfig {
    pub secret_shares: u8,
    pub secret_threshold: u8,
    pub initialized_at: DateTime<Utc>,
    /// `shamir`, or the auto-unseal type that wrapped the root key.
    #[serde(default = "shamir")]
    pub seal_type: String,
}

fn shamir() -> String {
    "shamir".into()
}

#[derive(Serialize)]
pub struct SealStatus {
    pub initialized: bool,
    pub sealed: bool,
    /// Shares required to unseal (0 when uninitialized).
    pub t: u8,
    /// Shares issued at init.
    pub n: u8,
    /// Shares submitted so far in the current unseal attempt.
    pub progress: u8,
    /// `redis` or `raft`.
    pub storage: &'static str,
    /// Raft node name (raft only).
    pub node: Option<String>,
    /// Set while this node is waiting to join a Raft cluster through that address.
    pub joining: Option<String>,
    /// `shamir` or the auto-unseal type (`transit`, `awskms`, `static`).
    pub seal_type: String,
    /// This instance's configured key service, if any.
    pub auto_unseal: Option<String>,
    /// Why the last auto-unseal attempt failed (key service down, wrong key…).
    pub auto_unseal_error: Option<String>,
    /// Sealed on purpose: auto-unseal is paused until this instance restarts.
    pub auto_unseal_paused: bool,
}

#[derive(Serialize)]
pub struct InitResult {
    /// base64 unseal shares — shown exactly once, never stored. Empty with auto-unseal.
    pub keys: Vec<String>,
    pub root_token: String,
    pub seal_type: String,
}

#[derive(Serialize, Deserialize)]
pub struct JoinChallenge {
    pub seal_config: SealConfig,
    /// The cluster's keyring blob, encrypted with the root key (as stored).
    pub keyring: String,
    /// A random nonce encrypted by the cluster's barrier.
    pub challenge: String,
    /// Auto-unseal: the wrapped root key, so a joiner with access to the same
    /// key service can unseal — and therefore join — on its own.
    #[serde(default)]
    pub wrapped_root: Option<WrappedKey>,
}

/// A fresh Raft node that found a cluster but isn't a member yet. It knows the
/// cluster's seal config and root-key-encrypted keyring, so it can be unsealed
/// with the cluster's keys — which is exactly what proves it may join.
#[derive(Clone)]
struct PendingJoin {
    via: String,
    seal_config: SealConfig,
    keyring: Vec<u8>,
    challenge: Vec<u8>,
    wrapped_root: Option<WrappedKey>,
    fetched: Instant,
}

struct Unsealed {
    /// When this instance was unsealed — a seal-all older than this doesn't apply.
    unsealed_at: DateTime<Utc>,
    root_key: Key,
    barrier: Barrier,
    /// The stored keyring blob this barrier was loaded from — used to notice a
    /// rotation replicated from another node.
    keyring_blob: Vec<u8>,
}

/// This process, as other replicas and operators see it.
#[derive(Clone)]
pub struct Identity {
    pub id: String,
    pub api_addr: String,
    pub started_at: DateTime<Utc>,
}

pub struct Core {
    pub storage: Storage,
    pub identity: Identity,
    unsealed: RwLock<Option<Unsealed>>,
    progress: Mutex<Vec<Zeroizing<Vec<u8>>>>,
    /// Serializes read-modify-write cycles (KV versioning, keyring rotation)
    /// within this process. With Raft, all writes are forwarded to the leader,
    /// so this still serializes the whole cluster.
    pub write_lock: Mutex<()>,
    pending_join: Mutex<Option<PendingJoin>>,
    /// Configured key service for auto-unseal (None = Shamir).
    auto: Option<AutoSeal>,
    /// Set by an explicit seal (incl. seal-all): auto-unseal then stays off
    /// until the process restarts, or an emergency seal would undo itself.
    sealed_on_purpose: AtomicBool,
    auto_error: Mutex<Option<String>>,
}

fn join_path(node: &str) -> String {
    format!("core/raft-join/{node}")
}

fn challenge_path(node: &str) -> String {
    format!("core/raft-challenge/{node}")
}

impl Core {
    pub fn new(storage: Storage, identity: Identity, auto: Option<AutoSeal>) -> Self {
        Self {
            auto,
            sealed_on_purpose: AtomicBool::new(false),
            auto_error: Mutex::new(None),
            storage,
            identity,
            unsealed: RwLock::new(None),
            progress: Mutex::new(Vec::new()),
            write_lock: Mutex::new(()),
            pending_join: Mutex::new(None),
        }
    }

    pub async fn seal_config(&self) -> AppResult<Option<SealConfig>> {
        match self.storage.get_local(SEAL_CONFIG_PATH).await? {
            Some(raw) => Ok(Some(serde_json::from_slice(&raw)?)),
            None => Ok(self.pending_join.lock().await.as_ref().map(|p| p.seal_config.clone())),
        }
    }

    pub async fn is_sealed(&self) -> bool {
        self.unsealed.read().await.is_none()
    }

    pub async fn status(&self) -> AppResult<SealStatus> {
        let cfg = self.seal_config().await?;
        Ok(SealStatus {
            initialized: cfg.is_some(),
            sealed: self.is_sealed().await,
            t: cfg.as_ref().map_or(0, |c| c.secret_threshold),
            n: cfg.as_ref().map_or(0, |c| c.secret_shares),
            progress: self.progress.lock().await.len() as u8,
            storage: self.storage.kind(),
            node: self.storage.raft().map(|r| r.cfg.node.name.clone()),
            joining: self.pending_join.lock().await.as_ref().map(|p| p.via.clone()),
            seal_type: cfg
                .as_ref()
                .map(|c| c.seal_type.clone())
                .unwrap_or_else(|| self.auto.as_ref().map_or("shamir", |a| a.kind()).into()),
            auto_unseal: self.auto.as_ref().map(|a| a.describe()),
            auto_unseal_error: self.auto_error.lock().await.clone(),
            auto_unseal_paused: self.auto.is_some() && self.sealed_on_purpose.load(Ordering::SeqCst),
        })
    }

    /// Generate the root key + keyring + root token, split the root key into
    /// shares, and persist everything in one atomic commit. The vault stays
    /// sealed afterwards, exactly like `vault operator init`. With Raft, this
    /// node first bootstraps itself as a one-voter cluster.
    pub async fn initialize(&self, shares: u8, threshold: u8) -> AppResult<InitResult> {
        let (shares, threshold) = if self.auto.is_some() { (0, 0) } else { (shares, threshold) };
        if self.auto.is_none() && (shares == 0 || threshold == 0 || threshold > shares || shares > 16) {
            return Err(AppError::BadRequest(
                "need 1 ≤ secret_threshold ≤ secret_shares ≤ 16".into(),
            ));
        }
        let _guard = self.write_lock.lock().await;
        if let Some(p) = self.pending_join.lock().await.as_ref() {
            return Err(AppError::BadRequest(format!(
                "this node found an existing cluster via {} — unseal it with that cluster's keys instead of initializing",
                p.via
            )));
        }
        if self.seal_config().await?.is_some() {
            return Err(AppError::BadRequest("vault is already initialized".into()));
        }

        let root_key = barrier::random_key();
        let keyring = Barrier::fresh();
        let keyring_blob = barrier::encrypt_with(
            &root_key,
            ROOT_TERM,
            KEYRING_PATH,
            &serde_json::to_vec(&keyring.to_doc())?,
        );
        let (root_token, token_path, token_entry) = token::new_root()?;
        let token_blob = keyring.encrypt(&token_path, &token_entry);
        let seal_type = self.auto.as_ref().map_or("shamir", |a| a.kind()).to_string();
        let cfg = SealConfig {
            secret_shares: shares,
            secret_threshold: threshold,
            initialized_at: Utc::now(),
            seal_type: seal_type.clone(),
        };
        // Auto-unseal: wrap the root key *before* writing anything, so a key
        // service problem fails init cleanly instead of half-initializing.
        let wrapped = match &self.auto {
            Some(a) => Some(
                a.wrap(root_key.as_slice())
                    .await
                    .map_err(|e| AppError::Unavailable(format!("auto-unseal key service: {e}")))?,
            ),
            None => None,
        };

        if let Some(raft) = self.storage.raft() {
            raft.bootstrap(cluster_auth::derive_key(&root_key)).await?;
        }
        let seal_cfg_bytes = serde_json::to_vec(&cfg)?;
        // Guards make init race-safe across instances sharing storage: exactly one
        // can write the keyring + seal config; the others get WriteConflict.
        let mut puts = vec![
            (KEYRING_PATH.to_string(), keyring_blob),
            (token_path.clone(), token_blob),
            (SEAL_CONFIG_PATH.to_string(), seal_cfg_bytes.clone()),
        ];
        if let Some(w) = &wrapped {
            puts.push((WRAPPED_ROOT_PATH.to_string(), serde_json::to_vec(w)?));
        }
        let written = self
            .storage
            .commit(
                &puts,
                &[],
                &[
                    Guard { path: SEAL_CONFIG_PATH.into(), expect: None },
                    Guard { path: KEYRING_PATH.into(), expect: None },
                ],
            )
            .await;
        match &written {
            Err(AppError::WriteConflict) => {
                if let Some(raft) = self.storage.raft() {
                    raft.stop().await;
                }
                return Err(AppError::BadRequest("vault was initialized concurrently by another instance".into()));
            }
            Err(e) => {
                // The write may have landed even though it "failed" (e.g. Redis WAIT
                // timed out after the primary applied it). The keys below were never
                // shown to anyone, so leaving that data would brick the vault:
                // "already initialized", with no way to unseal. Undo it — but only if
                // it's still *our* init (guarded), never another instance's.
                tracing::warn!("init failed ({e}); rolling back so it can be retried");
                let rollback =
                    [SEAL_CONFIG_PATH.to_string(), KEYRING_PATH.to_string(), WRAPPED_ROOT_PATH.to_string(), token_path];
                let ours = [Guard { path: SEAL_CONFIG_PATH.into(), expect: Some(seal_cfg_bytes) }];
                if let Err(re) = self.storage.commit(&[], &rollback, &ours).await {
                    // A replica-confirmation timeout here still means the deletes ran on the primary.
                    tracing::error!("init rollback reported: {re} — if storage is unreachable it may hold a half-initialized vault");
                }
            }
            Ok(()) => {}
        }
        // Sealed after init: stop Raft until the operators unseal.
        if let Some(raft) = self.storage.raft() {
            raft.stop().await;
        }
        written?;

        if self.auto.is_some() {
            // Like Vault: with auto-unseal the vault is unsealed right after init
            // and no unseal keys exist to hand out.
            tracing::info!("vault initialized with {seal_type} auto-unseal");
            self.open_with_root_key(root_key, "unexpected: fresh keyring did not decrypt").await?;
            return Ok(InitResult { keys: vec![], root_token, seal_type });
        }
        let keys = Sharks(threshold)
            .dealer(root_key.as_slice())
            .take(shares as usize)
            .map(|s| B64.encode(Vec::from(&s)))
            .collect();

        tracing::info!("vault initialized with {shares} shares, threshold {threshold}");
        Ok(InitResult { keys, root_token, seal_type })
    }

    /// Submit one unseal share. Once `threshold` distinct shares are in, the
    /// root key is reconstructed and used to decrypt the keyring. With Raft the
    /// node then starts replicating, and — if it was waiting to join — answers
    /// the leader's challenge to be added as a voter.
    pub async fn unseal(&self, share_b64: &str) -> AppResult<SealStatus> {
        self.discover_now_if_joining().await;
        let cfg =self.seal_config().await?.ok_or(AppError::Uninitialized)?;
        if !self.is_sealed().await {
            return self.status().await;
        }
        if cfg.seal_type != "shamir" {
            return Err(AppError::BadRequest(format!(
                "this vault uses {} auto-unseal — instances unseal themselves. \
                 One that was sealed on purpose stays sealed until it is restarted.",
                cfg.seal_type
            )));
        }
        let share = Zeroizing::new(
            B64.decode(share_b64.trim())
                .map_err(|_| AppError::BadRequest("unseal key is not valid base64".into()))?,
        );
        Share::try_from(share.as_slice())
            .map_err(|_| AppError::BadRequest("unseal key is malformed".into()))?;

        let mut progress = self.progress.lock().await;
        if !progress.iter().any(|p| p.as_slice() == share.as_slice()) {
            progress.push(share);
        }
        if progress.len() < cfg.secret_threshold as usize {
            drop(progress);
            return self.status().await;
        }

        // Threshold reached: always clear progress, whatever the outcome.
        let submitted = std::mem::take(&mut *progress);
        drop(progress);
        let shares: Vec<Share> = submitted
            .iter()
            .filter_map(|s| Share::try_from(s.as_slice()).ok())
            .collect();
        let recovered = Zeroizing::new(
            Sharks(cfg.secret_threshold)
                .recover(&shares)
                .map_err(|e| AppError::BadRequest(format!("could not combine shares: {e}")))?,
        );
        let root_key: [u8; 32] = recovered
            .as_slice()
            .try_into()
            .map_err(|_| AppError::BadRequest("shares do not reconstruct a root key".into()))?;
        let root_key = Zeroizing::new(root_key);

        self.open_with_root_key(root_key.clone(), "unseal failed: these shares do not match this vault — progress reset")
            .await?;
        if self.auto.is_some() {
            self.migrate_to_auto(&root_key, &cfg).await;
        }
        self.status().await
    }

    /// Decrypt the keyring with the root key and bring this instance up:
    /// start Raft (and finish a pending join) when Raft is the backend.
    async fn open_with_root_key(&self, root_key: Key, mismatch: &str) -> AppResult<()> {
        let pending = self.pending_join.lock().await.clone();
        let keyring_blob = match (&pending, self.storage.get_local(KEYRING_PATH).await?) {
            (_, Some(local)) => local,
            (Some(p), None) => p.keyring.clone(),
            (None, None) => return Err(AppError::Internal("keyring missing from storage".into())),
        };
        let doc = barrier::decrypt_with(&root_key, KEYRING_PATH, &keyring_blob)
            .map_err(|_| AppError::BadRequest(mismatch.into()))?;
        let barrier = Barrier::from_doc(serde_json::from_slice::<KeyringDoc>(&doc)?)?;

        if let Some(raft) = self.storage.raft() {
            raft.start(cluster_auth::derive_key(&root_key)).await?;
            if let Some(p) = pending {
                if let Err(e) = self.answer_join(&p, &barrier).await {
                    raft.stop().await;
                    // The leader consumed the challenge; drop it so retry_join fetches a fresh one.
                    *self.pending_join.lock().await = None;
                    return Err(AppError::BadRequest(format!(
                        "unsealed, but joining the cluster via {} failed: {e}. Will retry.",
                        p.via
                    )));
                }
                *self.pending_join.lock().await = None;
            }
        }

        *self.unsealed.write().await = Some(Unsealed { unsealed_at: Utc::now(), root_key, barrier, keyring_blob });
        tracing::info!("vault unsealed");
        Ok(())
    }

    /// Shamir → auto-unseal migration: after operators unseal with their shares
    /// once, wrap the root key with the configured key service and switch the
    /// vault over. Other instances then auto-unseal on their next attempt.
    async fn migrate_to_auto(&self, root_key: &Key, cfg: &SealConfig) {
        let Some(auto) = &self.auto else { return };
        let res: AppResult<()> = async {
            let old_cfg = self
                .storage
                .get_local(SEAL_CONFIG_PATH)
                .await?
                .ok_or_else(|| AppError::Internal("seal config missing".into()))?;
            let wrapped = auto
                .wrap(root_key.as_slice())
                .await
                .map_err(|e| AppError::Unavailable(format!("auto-unseal key service: {e}")))?;
            let new_cfg = SealConfig { secret_shares: 0, secret_threshold: 0, seal_type: auto.kind().into(), ..cfg.clone() };
            self.storage
                .commit(
                    &[
                        (WRAPPED_ROOT_PATH.into(), serde_json::to_vec(&wrapped)?),
                        (SEAL_CONFIG_PATH.into(), serde_json::to_vec(&new_cfg)?),
                    ],
                    &[],
                    &[Guard { path: SEAL_CONFIG_PATH.into(), expect: Some(old_cfg) }],
                )
                .await
        }
        .await;
        match res {
            Ok(()) => {
                *self.auto_error.lock().await = None;
                tracing::warn!("seal migrated: shamir → {} ({}); unseal keys are no longer used", auto.kind(), auto.describe())
            }
            Err(e) => {
                tracing::error!("seal migration to {} failed: {e}", auto.kind());
                *self.auto_error.lock().await = Some(format!("migration failed: {e}"));
            }
        }
    }

    /// One auto-unseal attempt: unwrap the root key through the key service.
    pub async fn try_auto_unseal(&self) -> AppResult<bool> {
        let Some(auto) = &self.auto else { return Ok(false) };
        if !self.is_sealed().await || self.sealed_on_purpose.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let Some(cfg) = self.seal_config().await? else { return Ok(false) };
        if cfg.seal_type == "shamir" {
            return Err(AppError::BadRequest(format!(
                "vault still uses Shamir keys — unseal once with them to migrate to {}",
                auto.kind()
            )));
        }
        let wrapped: WrappedKey = match self.storage.get_local(WRAPPED_ROOT_PATH).await? {
            Some(raw) => serde_json::from_slice(&raw)?,
            None => self
                .pending_join
                .lock()
                .await
                .as_ref()
                .and_then(|p| p.wrapped_root.clone())
                .ok_or_else(|| AppError::Internal("wrapped root key missing from storage".into()))?,
        };
        let raw = Zeroizing::new(auto.unwrap(&wrapped).await.map_err(|e| AppError::Unavailable(e.to_string()))?);
        let root_key: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Internal("key service returned a key of the wrong length".into()))?;
        self.open_with_root_key(Zeroizing::new(root_key), "the key service returned a root key that does not match this vault")
            .await?;
        Ok(true)
    }

    /// Keep trying to auto-unseal while sealed (key service may be briefly down,
    /// or a Raft joiner may still be waiting for its challenge).
    pub fn spawn_auto_unseal(self: &Arc<Self>) {
        let Some(auto) = &self.auto else { return };
        tracing::info!("auto-unseal enabled: {}", auto.describe());
        let core = self.clone();
        tokio::spawn(async move {
            loop {
                match core.try_auto_unseal().await {
                    Ok(true) => {
                        tracing::info!("auto-unsealed via {}", core.auto.as_ref().map(|a| a.kind()).unwrap_or(""));
                        *core.auto_error.lock().await = None;
                    }
                    Ok(false) => {
                        if !core.is_sealed().await {
                            *core.auto_error.lock().await = None;
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        let mut last = core.auto_error.lock().await;
                        if last.as_deref() != Some(msg.as_str()) {
                            tracing::warn!("auto-unseal: {msg}");
                        }
                        *last = Some(msg);
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    pub async fn unseal_reset(&self) -> AppResult<SealStatus> {
        self.progress.lock().await.clear();
        self.status().await
    }

    /// Drop the in-memory keys (and leave Raft). Stored data is untouched but unreadable.
    pub async fn seal(&self) {
        self.sealed_on_purpose.store(true, Ordering::SeqCst);
        *self.unsealed.write().await = None;
        self.progress.lock().await.clear();
        if let Some(raft) = self.storage.raft() {
            raft.stop().await;
        }
        tracing::info!("vault sealed");
    }

    /// HMAC used to identify tokens in the audit log without revealing them.
    /// Keyed from the root key, so every instance produces the same value.
    pub async fn audit_hmac(&self, data: &[u8]) -> Option<String> {
        let key = self.derive_key(b"timika audit hmac v1").await?;
        Some(format!("hmac-sha256:{}", cluster_auth::tag(&key, data)))
    }

    /// A purpose-specific key derived from the root key: identical on every
    /// instance, available only while unsealed, never stored.
    pub async fn derive_key(&self, purpose: &[u8]) -> Option<[u8; 32]> {
        let g = self.unsealed.read().await;
        let base = cluster_auth::derive_key(&g.as_ref()?.root_key);
        hex::decode(cluster_auth::tag(&base, purpose)).ok()?.try_into().ok()
    }

    /// Seal **every** instance, not just this one — what you want in an
    /// emergency when N replicas sit behind a load balancer.
    ///
    /// - Redis: write a marker signed with the cluster key; every unsealed
    ///   instance checks it on its next heartbeat (≤3s) and seals.
    /// - Raft: send a signed `/raft/seal` to every peer directly.
    pub async fn seal_all(&self) -> AppResult<Value> {
        let key = {
            let g = self.unsealed.read().await;
            cluster_auth::derive_key(&g.as_ref().ok_or(AppError::Sealed)?.root_key)
        };
        let report = match &self.storage {
            Storage::Redis(r) => {
                let at = Utc::now().timestamp_millis();
                let tag = cluster_auth::tag(&key, format!("seal-all:{at}").as_bytes());
                r.ops_set("seal-all", &json!({ "at": at, "sig": tag }).to_string()).await?;
                json!({ "mechanism": "redis", "note": "every unsealed instance seals within ~3s" })
            }
            Storage::Raft(r) => json!({ "mechanism": "raft", "peers": r.broadcast_seal().await }),
        };
        self.seal().await;
        tracing::warn!("seal-all issued by {}", self.identity.id);
        Ok(report)
    }

    /// Redis scale-out: heartbeat into the instance registry every 3s, and obey
    /// a valid seal-all marker newer than our own unseal.
    pub fn spawn_heartbeat(self: &Arc<Self>) {
        let Storage::Redis(redis) = &self.storage else { return };
        let (core, redis) = (self.clone(), redis.clone());
        tokio::spawn(async move {
            loop {
                // Obey seal-all first, so the heartbeat below reports the state we're really in.
                if let Err(e) = core.check_seal_all(&redis).await {
                    tracing::debug!("seal-all check failed: {e}");
                }
                let status = core.status().await.ok();
                let entry = json!({
                    "id": core.identity.id,
                    "api_addr": core.identity.api_addr,
                    "sealed": status.as_ref().map(|s| s.sealed),
                    "initialized": status.as_ref().map(|s| s.initialized),
                    "version": env!("CARGO_PKG_VERSION"),
                    "started_at": core.identity.started_at,
                    "last_seen": Utc::now().timestamp(),
                });
                if let Err(e) = redis.registry_beat(&core.identity.id, &entry, 30).await {
                    tracing::debug!("heartbeat failed: {e}");
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    async fn check_seal_all(&self, redis: &crate::storage::RedisStorage) -> AppResult<()> {
        let Some(raw) = redis.ops_get("seal-all").await? else { return Ok(()) };
        let marker: Value = serde_json::from_str(&raw)?;
        let (Some(at), Some(sig)) = (marker["at"].as_i64(), marker["sig"].as_str()) else { return Ok(()) };
        let should = {
            let g = self.unsealed.read().await;
            let Some(u) = g.as_ref() else { return Ok(()) };
            at > u.unsealed_at.timestamp_millis()
                && cluster_auth::check(&cluster_auth::derive_key(&u.root_key), format!("seal-all:{at}").as_bytes(), sig)
        };
        if should {
            tracing::warn!("seal-all marker found — sealing this instance");
            self.seal().await;
        }
        Ok(())
    }

    /// Every instance serving this vault and whether it's sealed — public, like
    /// seal-status, so operators can see what still needs unsealing.
    pub async fn instances(&self) -> AppResult<Value> {
        let me = self.status().await?;
        let mut list: Vec<Value> = match &self.storage {
            Storage::Redis(r) => {
                let now = Utc::now().timestamp();
                r.registry_list()
                    .await?
                    .into_iter()
                    .map(|mut v| {
                        let seen = v["last_seen"].as_i64().unwrap_or(0);
                        v["stale"] = json!(now - seen > 15);
                        v["self"] = json!(v["id"] == json!(self.identity.id));
                        v
                    })
                    .collect()
            }
            Storage::Raft(r) => {
                let peers = r.peers().await;
                let mut probes = Vec::new();
                for p in peers {
                    let is_self = p["self"] == true;
                    probes.push(tokio::spawn(async move {
                        let sealed = if is_self {
                            None
                        } else {
                            crate::grpc::client::seal_status(p["api_addr"].as_str().unwrap_or_default())
                                .await
                                .map(|s| s.sealed)
                        };
                        json!({
                            "id": p["name"], "api_addr": p["api_addr"], "leader": p["leader"],
                            "voter": p["voter"], "self": is_self, "sealed": sealed,
                            "stale": !is_self && sealed.is_none(),
                        })
                    }));
                }
                let mut out = Vec::new();
                for p in probes {
                    if let Ok(v) = p.await {
                        out.push(v);
                    }
                }
                // Configured retry_join peers that aren't members yet (waiting to be
                // unsealed so they can join) — so `unseal --all` reaches them too.
                let known: Vec<String> = out.iter().filter_map(|v| v["api_addr"].as_str().map(String::from)).collect();
                for addr in r.cfg.retry_join.iter().filter(|a| !known.contains(a) && **a != self.identity.api_addr) {
                    let st = crate::grpc::client::seal_status(addr).await;
                    out.push(json!({
                        "id": st.as_ref().and_then(|s| s.node.clone()).unwrap_or_else(|| addr.clone()),
                        "api_addr": addr, "leader": false, "voter": false, "self": false,
                        "sealed": st.as_ref().map(|s| s.sealed),
                        "joining": true,
                        "stale": st.is_none(),
                    }));
                }
                out
            }
        };
        for v in list.iter_mut().filter(|v| v["self"] == true) {
            v["sealed"] = json!(me.sealed);
        }
        list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(json!({ "storage": self.storage.kind(), "self": self.identity.id, "instances": list }))
    }

    /// Add a new data-encryption key term. Existing blobs are not rewritten;
    /// they stay readable via their old term, new writes use the new one.
    pub async fn rotate(&self) -> AppResult<KeyStatus> {
        let _guard = self.write_lock.lock().await;
        for _ in 0..MAX_RETRIES {
            self.refresh_keyring().await?;
            // Build the rotated keyring on a copy: the live barrier only changes
            // once storage has accepted it, so a failed write can never leave this
            // instance encrypting with a key that was never persisted.
            let (next, blob, current) = {
                let g = self.unsealed.read().await;
                let u = g.as_ref().ok_or(AppError::Sealed)?;
                let mut next = Barrier::from_doc(u.barrier.to_doc())?;
                next.rotate();
                let blob = barrier::encrypt_with(&u.root_key, ROOT_TERM, KEYRING_PATH, &serde_json::to_vec(&next.to_doc())?);
                (next, blob, u.keyring_blob.clone())
            };
            let guard = Guard { path: KEYRING_PATH.into(), expect: Some(current) };
            match self.storage.commit(&[(KEYRING_PATH.into(), blob.clone())], &[], &[guard]).await {
                Ok(()) => {
                    if let Some(u) = self.unsealed.write().await.as_mut() {
                        u.barrier = next;
                        u.keyring_blob = blob;
                    }
                    return self.key_status().await;
                }
                // Another instance rotated first: reload and rotate on top of it.
                Err(AppError::WriteConflict) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(AppError::WriteConflict)
    }

    pub async fn key_status(&self) -> AppResult<KeyStatus> {
        self.refresh_keyring().await?;
        let g = self.unsealed.read().await;
        let u = g.as_ref().ok_or(AppError::Sealed)?;
        Ok(KeyStatus {
            term: u.barrier.active_term(),
            install_time: u.barrier.installed_at(),
            terms: u.barrier.term_count(),
            encryption: "aes-256-gcm",
        })
    }

    /// Reload the keyring if the stored copy changed — e.g. another node rotated
    /// while this one was a follower, and this node is now the leader.
    async fn refresh_keyring(&self) -> AppResult<()> {
        let Some(blob) = self.storage.get_local(KEYRING_PATH).await? else { return Ok(()) };
        {
            let g = self.unsealed.read().await;
            if g.as_ref().ok_or(AppError::Sealed)?.keyring_blob == blob {
                return Ok(());
            }
        }
        let mut g = self.unsealed.write().await;
        let u = g.as_mut().ok_or(AppError::Sealed)?;
        let doc = barrier::decrypt_with(&u.root_key, KEYRING_PATH, &blob)?;
        u.barrier = Barrier::from_doc(serde_json::from_slice::<KeyringDoc>(&doc)?)?;
        u.keyring_blob = blob;
        tracing::info!("keyring reloaded (active term {})", u.barrier.active_term());
        Ok(())
    }

    // ── Barrier-wrapped storage: plaintext in, ciphertext to storage ──────────

    /// Decrypted value plus the stored blob it came from (for a commit guard).
    async fn read(&self, path: &str) -> AppResult<Option<(Vec<u8>, Vec<u8>)>> {
        if self.is_sealed().await {
            return Err(AppError::Sealed);
        }
        let Some(blob) = self.storage.get(path).await? else { return Ok(None) };
        let term = barrier::blob_term(&blob)?;
        let known = {
            let g = self.unsealed.read().await;
            g.as_ref().ok_or(AppError::Sealed)?.barrier.has_term(term)
        };
        if !known {
            self.refresh_keyring().await?;
        }
        let g = self.unsealed.read().await;
        let plain = g.as_ref().ok_or(AppError::Sealed)?.barrier.decrypt(path, &blob)?;
        Ok(Some((plain, blob)))
    }

    pub async fn get(&self, path: &str) -> AppResult<Option<Vec<u8>>> {
        Ok(self.read(path).await?.map(|(plain, _)| plain))
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> AppResult<Option<T>> {
        match self.get(path).await? {
            Some(raw) => Ok(Some(serde_json::from_slice(&raw)?)),
            None => Ok(None),
        }
    }

    /// Read a JSON value for a read-modify-write: returns it together with a
    /// guard that makes the later commit fail if anyone changed it meanwhile.
    pub async fn get_json_guarded<T: serde::de::DeserializeOwned>(&self, path: &str) -> AppResult<(Option<T>, Guard)> {
        match self.read(path).await? {
            Some((plain, blob)) => Ok((Some(serde_json::from_slice(&plain)?), Guard { path: path.into(), expect: Some(blob) })),
            None => Ok((None, Guard { path: path.into(), expect: None })),
        }
    }

    /// Encrypt and atomically write `puts`, deleting `deletes` in the same
    /// commit — only if every guard still holds (else `WriteConflict`).
    pub async fn commit(&self, puts: Vec<(String, Vec<u8>)>, deletes: Vec<String>, guards: Vec<Guard>) -> AppResult<()> {
        self.refresh_keyring().await?;
        let sealed_puts: Vec<(String, Vec<u8>)> = {
            let g = self.unsealed.read().await;
            let b = &g.as_ref().ok_or(AppError::Sealed)?.barrier;
            puts.into_iter().map(|(p, v)| { let blob = b.encrypt(&p, &v); (p, blob) }).collect()
        };
        self.storage.commit(&sealed_puts, &deletes, &guards).await
    }

    pub async fn list(&self, prefix: &str) -> AppResult<Vec<String>> {
        if self.is_sealed().await {
            return Err(AppError::Sealed);
        }
        self.storage.list(prefix).await
    }

    // ── Raft join ─────────────────────────────────────────────────────────────

    /// Leader side, step 1: hand a would-be member what it needs to be unsealed
    /// with this cluster's keys, plus a nonce only the real keyring can decrypt.
    pub async fn join_challenge(&self, node: &ClusterNode) -> AppResult<JoinChallenge> {
        if self.storage.raft().is_none() {
            return Err(AppError::BadRequest("storage backend is not raft".into()));
        }
        let seal_config = self.seal_config().await?.ok_or(AppError::Uninitialized)?;
        let keyring = self
            .storage
            .get_local(KEYRING_PATH)
            .await?
            .ok_or_else(|| AppError::Internal("keyring missing".into()))?;
        let mut nonce = Zeroizing::new(vec![0u8; 32]);
        rand::thread_rng().fill_bytes(nonce.as_mut_slice());
        let challenge = {
            let g = self.unsealed.read().await;
            g.as_ref().ok_or(AppError::Sealed)?.barrier.encrypt(&challenge_path(&node.name), &nonce)
        };
        // Stored (encrypted) rather than in memory, so any node can verify the answer.
        let pending = json!({ "nonce": B64.encode(nonce.as_slice()), "at": Utc::now() });
        self.commit(vec![(join_path(&node.name), serde_json::to_vec(&pending)?)], vec![], vec![]).await?;
        tracing::info!("raft: issued join challenge to {}", node.name);
        let wrapped_root = match self.storage.get_local(WRAPPED_ROOT_PATH).await? {
            Some(raw) => Some(serde_json::from_slice(&raw)?),
            None => None,
        };
        Ok(JoinChallenge { seal_config, keyring: B64.encode(keyring), challenge: B64.encode(challenge), wrapped_root })
    }

    /// Leader side, step 2: the node proved it decrypted the challenge — it
    /// holds the unseal keys — so add it as a voter.
    pub async fn join_answer(&self, node: ClusterNode, answer_b64: &str) -> AppResult<()> {
        let raft = self.storage.raft().ok_or_else(|| AppError::BadRequest("storage backend is not raft".into()))?;
        let (pending, guard) = self.get_json_guarded::<Value>(&join_path(&node.name)).await?;
        let pending = pending.ok_or_else(|| AppError::BadRequest(format!("no pending join challenge for `{}`", node.name)))?;
        // Single use: consume it before checking, so a wrong answer can't be retried.
        self.commit(vec![], vec![join_path(&node.name)], vec![guard]).await?;
        let at: DateTime<Utc> = serde_json::from_value(pending["at"].clone())?;
        if (Utc::now() - at).num_seconds() > CHALLENGE_TTL.as_secs() as i64 {
            return Err(AppError::BadRequest("join challenge expired".into()));
        }
        let nonce = B64.decode(pending["nonce"].as_str().unwrap_or_default()).map_err(|_| AppError::Internal("bad stored nonce".into()))?;
        let answer = B64.decode(answer_b64).map_err(|_| AppError::BadRequest("answer is not base64".into()))?;
        if answer != nonce {
            return Err(AppError::Auth("join challenge answer is wrong".into()));
        }
        raft.add_voter(node).await
    }

    /// Joiner side: decrypt the challenge with the freshly unsealed barrier and
    /// send the answer back through the node we found the cluster with.
    async fn answer_join(&self, p: &PendingJoin, barrier: &Barrier) -> anyhow::Result<()> {
        let raft = self.storage.raft().ok_or_else(|| anyhow::anyhow!("not raft"))?;
        let nonce = barrier.decrypt(&challenge_path(&raft.cfg.node.name), &p.challenge)?;
        crate::grpc::client::join_answer(&p.via, &raft.cfg.node, &B64.encode(nonce)).await?;
        tracing::info!("raft: joined cluster via {}", p.via);
        Ok(())
    }

    async fn fetch_challenge(&self, via: &str, node: &ClusterNode) -> anyhow::Result<JoinChallenge> {
        crate::grpc::client::join_challenge(via, node).await
    }

    /// Make the next reads reflect every write completed before this call —
    /// on a Raft follower too (it asks the leader how far to catch up).
    /// gRPC handlers call it once per request; Redis needs nothing.
    pub async fn linearize(&self) -> AppResult<()> {
        match &self.storage {
            Storage::Raft(r) => r.linearize().await,
            Storage::Redis(_) => Ok(()),
        }
    }

    /// `retry_join`: until this node is a Raft member, keep asking the
    /// configured peers for a join challenge. Once one answers, the node shows
    /// as initialized + sealed; unsealing it with the cluster's keys completes
    /// the join.
    pub fn spawn_retry_join(self: &Arc<Self>) {
        let Some(raft) = self.storage.raft().cloned() else { return };
        if raft.cfg.retry_join.is_empty() {
            return;
        }
        let core = self.clone();
        tokio::spawn(async move {
            loop {
                if raft.is_member() {
                    *core.pending_join.lock().await = None;
                    tracing::info!("raft: node is a cluster member; retry_join finished");
                    break;
                }
                let stale = core
                    .pending_join
                    .lock()
                    .await
                    .as_ref()
                    .is_none_or(|p| p.fetched.elapsed() > CHALLENGE_TTL - Duration::from_secs(60));
                if stale && core.is_sealed().await {
                    core.discover_cluster().await;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    /// Ask the `retry_join` addresses for a join challenge; on success this node
    /// is "joining": it reports the cluster's seal config and can be unsealed
    /// with the cluster's keys.
    async fn discover_cluster(&self) {
        let Some(raft) = self.storage.raft() else { return };
        for via in raft.cfg.retry_join.iter().filter(|a| **a != raft.cfg.node.api_addr) {
            match self.fetch_challenge(via, &raft.cfg.node).await {
                Ok(ch) => {
                    let decoded = (B64.decode(&ch.keyring), B64.decode(&ch.challenge));
                    if let (Ok(keyring), Ok(challenge)) = decoded {
                        let first = self.pending_join.lock().await.is_none();
                        *self.pending_join.lock().await = Some(PendingJoin {
                            via: via.clone(),
                            seal_config: ch.seal_config,
                            keyring,
                            challenge,
                            wrapped_root: ch.wrapped_root,
                            fetched: Instant::now(),
                        });
                        if first {
                            tracing::info!("raft: found cluster via {via} — unseal this node with the cluster's keys to join");
                        }
                        return;
                    }
                }
                Err(e) => tracing::debug!("raft: retry_join via {via}: {e}"),
            }
        }
    }

    /// An unseal key arrived before the retry_join poll found the cluster (the
    /// operator was quicker than 5s): look now instead of "not initialized".
    async fn discover_now_if_joining(&self) {
        let Some(raft) = self.storage.raft() else { return };
        if raft.cfg.retry_join.is_empty() || raft.is_member() || self.pending_join.lock().await.is_some() {
            return;
        }
        if matches!(self.storage.get_local(SEAL_CONFIG_PATH).await, Ok(None)) {
            self.discover_cluster().await;
        }
    }
}

#[derive(Serialize)]
pub struct KeyStatus {
    pub term: u32,
    pub install_time: DateTime<Utc>,
    pub terms: usize,
    pub encryption: &'static str,
}
