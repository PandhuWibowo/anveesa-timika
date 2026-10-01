//! Storage backend: Redis — standalone, Sentinel or Cluster.
//!
//! This layer is deliberately dumb: it moves opaque byte blobs in and out of
//! Redis and knows nothing about encryption. Everything except
//! `core/seal-config` arrives here *already encrypted* by the barrier, so Redis
//! (and its AOF/RDB files, replicas and backups) only ever holds ciphertext.
//!
//! Key layout — identical in every topology:
//!
//!   {prefix}:<path>     one value per storage path, e.g. `{timika}:kv/meta/app/db`
//!   {prefix}#index      sorted set of every stored path (ZRANGEBYLEX listing)
//!   {prefix}#instances  hash: instance id → heartbeat JSON (addr, sealed, …)
//!   {prefix}#seal-all   signed "seal every instance unsealed before T" marker
//!
//! The last two are operational metadata for running several timika instances
//! against one Redis (scale-out). They hold no secrets and sit outside the
//! barrier on purpose: sealed instances must be able to read and write them.
//!
//! The `{prefix}` hash tag puts every key in the same Cluster slot, so a commit
//! (many keys) stays atomic under Redis Cluster too. Vault data is small; one
//! slot is plenty, and it means one shard's replicas hold a complete copy.
//!
//! Commits run as one Lua script (atomic everywhere, including Cluster), which
//! also maintains the index — listing is O(log n) instead of a keyspace SCAN.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use redis::aio::{ConnectionLike, ConnectionManager, ConnectionManagerConfig, MultiplexedConnection};
use redis::cluster::ClusterClientBuilder;
use redis::cluster_async::ClusterConnection;
use redis::cluster_routing::{get_slot, Route, RoutingInfo, SingleNodeRoutingInfo, SlotAddr};
use redis::sentinel::{SentinelClient, SentinelNodeConnectionInfo, SentinelServerType};
use redis::{
    AsyncConnectionConfig, Cmd, ErrorKind, FromRedisValue, Pipeline, RedisConnectionInfo, RedisError, RedisFuture, RedisResult, Script,
    Value,
};
use serde_json::{json, Value as Json};
use tokio::sync::{Mutex, RwLock};

use crate::config::{RedisConfig, RedisMode};
use crate::error::{AppError, AppResult};
use crate::storage::Guard;

/// KEYS = [index, guard keys…, put keys…, delete keys…]
/// ARGV = [guards, puts, namespace length, guard expectations…, put values…]
/// A guard expectation is "\0" (key must not exist) or "\1" + the exact blob.
/// Returns 1 when applied, 0 when a guard failed (nothing written).
const COMMIT_LUA: &str = r#"
local g = tonumber(ARGV[1])
local n = tonumber(ARGV[2])
local ns = tonumber(ARGV[3])
for i = 1, g do
  local cur = redis.call('GET', KEYS[i + 1])
  local want = ARGV[i + 3]
  if string.sub(want, 1, 1) == '\0' then
    if cur then return 0 end
  elseif cur ~= string.sub(want, 2) then
    return 0
  end
end
for i = 1, n do
  local k = KEYS[g + i + 1]
  redis.call('SET', k, ARGV[g + i + 3])
  redis.call('ZADD', KEYS[1], 0, string.sub(k, ns + 1))
end
for i = g + n + 2, #KEYS do
  redis.call('DEL', KEYS[i])
  redis.call('ZREM', KEYS[1], string.sub(KEYS[i], ns + 1))
end
return 1
"#;

/// One connection type for all three topologies, so every command path is shared.
/// Every variant is a cheap handle over shared (Arc'd) state, so the size gap
/// between them costs nothing worth boxing for.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum Conn {
    /// Auto-reconnecting single connection.
    Standalone(ConnectionManager),
    /// Connection to whichever node Sentinel says is the primary right now.
    Sentinel(MultiplexedConnection),
    /// Slot-aware; follows MOVED/ASK and topology changes on its own.
    Cluster(ClusterConnection),
}

impl ConnectionLike for Conn {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            Conn::Standalone(c) => c.req_packed_command(cmd),
            Conn::Sentinel(c) => c.req_packed_command(cmd),
            Conn::Cluster(c) => c.req_packed_command(cmd),
        }
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        cmd: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        match self {
            Conn::Standalone(c) => c.req_packed_commands(cmd, offset, count),
            Conn::Sentinel(c) => c.req_packed_commands(cmd, offset, count),
            Conn::Cluster(c) => c.req_packed_commands(cmd, offset, count),
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            Conn::Standalone(c) => c.get_db(),
            Conn::Sentinel(c) => c.get_db(),
            Conn::Cluster(c) => c.get_db(),
        }
    }
}

/// Errors that mean "you're not talking to a writable primary any more".
fn is_failover_error(e: &RedisError) -> bool {
    e.is_io_error() || e.is_connection_dropped() || e.is_connection_refusal() || e.kind() == ErrorKind::ReadOnly
}

pub struct RedisStorage {
    cfg: RedisConfig,
    conn: RwLock<Conn>,
    sentinel: Option<Mutex<SentinelClient>>,
    /// `{prefix}:`
    ns: String,
    /// `{prefix}#index`
    index: String,
    /// The Cluster slot every vault key hashes to.
    slot: u16,
    commit_script: Script,
}

impl RedisStorage {
    pub async fn connect(cfg: &RedisConfig) -> anyhow::Result<Self> {
        let tag = format!("{{{}}}", cfg.prefix);
        let (connect_timeout, response_timeout) = cfg.timeouts();
        let (conn, sentinel) = match cfg.mode {
            RedisMode::Standalone => {
                let client = redis::Client::open(cfg.url.as_str())?;
                let mcfg = ConnectionManagerConfig::new()
                    .set_connection_timeout(connect_timeout)
                    .set_response_timeout(response_timeout);
                (Conn::Standalone(ConnectionManager::new_with_config(client, mcfg).await?), None)
            }
            RedisMode::Sentinel => {
                if cfg.sentinels.is_empty() {
                    anyhow::bail!("REDIS_MODE=sentinel needs REDIS_SENTINELS");
                }
                let node = SentinelNodeConnectionInfo {
                    tls_mode: cfg.tls.then_some(redis::TlsMode::Secure),
                    redis_connection_info: Some(RedisConnectionInfo {
                        username: cfg.username.clone(),
                        password: cfg.password.clone(),
                        ..Default::default()
                    }),
                };
                let mut client = SentinelClient::build(
                    cfg.sentinels.clone(),
                    cfg.sentinel_master.clone(),
                    Some(node),
                    SentinelServerType::Master,
                )?;
                let c = client.get_async_connection_with_config(&cfg.async_config()).await?;
                (Conn::Sentinel(c), Some(Mutex::new(client)))
            }
            RedisMode::Cluster => {
                if cfg.cluster_nodes.is_empty() {
                    anyhow::bail!("REDIS_MODE=cluster needs REDIS_CLUSTER_NODES");
                }
                let mut b = ClusterClientBuilder::new(cfg.cluster_nodes.clone())
                    .connection_timeout(connect_timeout)
                    .response_timeout(response_timeout);
                if let Some(u) = &cfg.username {
                    b = b.username(u.clone());
                }
                if let Some(p) = &cfg.password {
                    b = b.password(p.clone());
                }
                (Conn::Cluster(b.build()?.get_async_connection().await?), None)
            }
        };
        let s = Self {
            cfg: cfg.clone(),
            conn: RwLock::new(conn),
            sentinel,
            ns: format!("{tag}:"),
            index: format!("{tag}#index"),
            slot: get_slot(tag.as_bytes()),
            commit_script: Script::new(COMMIT_LUA),
        };
        s.ensure_index().await?;
        Ok(s)
    }

    pub fn mode(&self) -> RedisMode {
        self.cfg.mode
    }

    fn key(&self, path: &str) -> String {
        format!("{}{}", self.ns, path)
    }

    async fn conn(&self) -> Conn {
        self.conn.read().await.clone()
    }

    /// Re-discover the primary through Sentinel and swap the connection.
    async fn reconnect(&self) -> RedisResult<()> {
        let Some(s) = &self.sentinel else { return Ok(()) };
        // Hard cap: while a failover is in progress Sentinel may still name the
        // dead primary, and redis-rs verifies its role over a connection with no
        // timeout — that would hang (and hold the lock) for a full TCP timeout.
        // Give up quickly; the watcher retries every 2s.
        let limit = self.cfg.timeouts().0 * 2;
        let attempt = async { s.lock().await.get_async_connection_with_config(&self.cfg.async_config()).await };
        let c = tokio::time::timeout(limit, attempt)
            .await
            .map_err(|_| RedisError::from((ErrorKind::IoError, "sentinel: primary discovery timed out")))??;
        *self.conn.write().await = Conn::Sentinel(c);
        tracing::info!("redis sentinel: (re)connected to the current primary of `{}`", self.cfg.sentinel_master);
        Ok(())
    }

    /// Run an operation; with Sentinel, if the primary went away or was demoted
    /// (READONLY), re-discover it and retry once.
    async fn run<T, F, Fut>(&self, f: F) -> RedisResult<T>
    where
        F: Fn(Conn) -> Fut,
        Fut: Future<Output = RedisResult<T>>,
    {
        match f(self.conn().await).await {
            Err(e) if self.sentinel.is_some() && is_failover_error(&e) => {
                tracing::warn!("redis sentinel: {e} — re-discovering primary");
                self.reconnect().await?;
                f(self.conn().await).await
            }
            r => r,
        }
    }

    /// A key-less command, sent to the node that owns our data. On Cluster
    /// that's the primary of our slot, not a random node.
    async fn on_primary<T: FromRedisValue>(&self, cmd: Cmd) -> RedisResult<T> {
        let slot = self.slot;
        self.run(|mut c| {
            let cmd = cmd.clone();
            async move {
                match &mut c {
                    Conn::Cluster(cc) => {
                        let route = SingleNodeRoutingInfo::SpecificNode(Route::new(slot, SlotAddr::Master));
                        let v = cc.route_command(&cmd, RoutingInfo::SingleNode(route)).await?;
                        T::from_redis_value(&v)
                    }
                    other => cmd.query_async(other).await,
                }
            }
        })
        .await
    }

    /// Sentinel only: every 2s make sure we're still connected to a primary,
    /// so a failover is noticed even when nobody is writing.
    pub fn spawn_sentinel_watch(self: &Arc<Self>) {
        if self.sentinel.is_none() {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let role = me.info("replication").await.ok().and_then(|m| m.get("role").cloned());
                if role.as_deref() != Some("master") {
                    tracing::warn!("redis sentinel: connected node role is {:?}; re-discovering primary", role);
                    if let Err(e) = me.reconnect().await {
                        tracing::error!("redis sentinel: no primary available: {e}");
                    }
                }
            }
        });
    }

    // ── data path ────────────────────────────────────────────────────────────

    pub async fn get(&self, path: &str) -> AppResult<Option<Vec<u8>>> {
        let key = self.key(path);
        Ok(self
            .run(|mut c| {
                let key = key.clone();
                async move { redis::cmd("GET").arg(key).query_async(&mut c).await }
            })
            .await?)
    }

    /// Apply puts and deletes atomically (one Lua script) if every guard holds,
    /// then — if configured — wait for replicas to confirm before acknowledging.
    pub async fn commit(&self, puts: &[(String, Vec<u8>)], deletes: &[String], guards: &[Guard]) -> AppResult<()> {
        let applied = self
            .run(|mut c| async move {
                let mut inv = self.commit_script.prepare_invoke();
                inv.key(&self.index);
                for g in guards {
                    inv.key(self.key(&g.path));
                }
                for (p, _) in puts {
                    inv.key(self.key(p));
                }
                for p in deletes {
                    inv.key(self.key(p));
                }
                inv.arg(guards.len()).arg(puts.len()).arg(self.ns.len());
                for g in guards {
                    let want = match &g.expect {
                        None => vec![0u8],
                        Some(b) => [&[1u8][..], b].concat(),
                    };
                    inv.arg(want);
                }
                for (_, v) in puts {
                    inv.arg(v.as_slice());
                }
                inv.invoke_async::<i64>(&mut c).await
            })
            .await?;
        if applied != 1 {
            return Err(AppError::WriteConflict);
        }

        let want = self.cfg.wait_replicas;
        if want > 0 {
            let acked: i64 = self
                .on_primary(redis::cmd("WAIT").arg(want).arg(self.cfg.wait_timeout_ms).clone())
                .await?;
            if acked < want as i64 {
                return Err(AppError::Unavailable(format!(
                    "write reached the primary but only {acked}/{want} replicas confirmed within {}ms",
                    self.cfg.wait_timeout_ms
                )));
            }
        }
        Ok(())
    }

    /// Every stored path under `prefix`, sorted — straight from the index.
    pub async fn list(&self, prefix: &str) -> AppResult<Vec<String>> {
        let mut min = b"[".to_vec();
        min.extend_from_slice(prefix.as_bytes());
        let mut max = min.clone();
        max.push(0xff);
        let index = self.index.clone();
        Ok(self
            .run(|mut c| {
                let (index, min, max) = (index.clone(), min.clone(), max.clone());
                async move { redis::cmd("ZRANGEBYLEX").arg(index).arg(min).arg(max).query_async(&mut c).await }
            })
            .await?)
    }

    pub async fn ping(&self) -> AppResult<()> {
        self.on_primary::<String>(redis::cmd("PING").clone()).await?;
        Ok(())
    }

    /// Build the index for data written before it existed, and move keys from
    /// the pre-hash-tag layout (`prefix:path`) to `{prefix}:path`.
    async fn ensure_index(&self) -> RedisResult<()> {
        let exists: i64 = self.on_primary(redis::cmd("EXISTS").arg(&self.index).clone()).await?;
        if exists == 1 {
            return Ok(());
        }
        if self.cfg.mode != RedisMode::Cluster {
            let legacy = self.scan(&format!("{}:*", glob_escape(&self.cfg.prefix))).await?;
            for old in &legacy {
                let new = format!("{}{}", self.ns, &old[self.cfg.prefix.len() + 1..]);
                self.on_primary::<()>(redis::cmd("RENAME").arg(old).arg(&new).clone()).await?;
            }
            if !legacy.is_empty() {
                tracing::info!("redis: migrated {} keys to the `{}` hash-tag layout", legacy.len(), self.ns);
            }
        }
        let keys = self.scan(&format!("{}*", glob_escape(&self.ns))).await?;
        for chunk in keys.chunks(500) {
            let mut cmd = redis::cmd("ZADD");
            cmd.arg(&self.index);
            for k in chunk {
                cmd.arg(0).arg(&k[self.ns.len()..]);
            }
            self.on_primary::<i64>(cmd).await?;
        }
        if !keys.is_empty() {
            tracing::info!("redis: indexed {} existing keys", keys.len());
        }
        Ok(())
    }

    /// SCAN on the node that owns our slot (startup migration only).
    async fn scan(&self, pattern: &str) -> RedisResult<Vec<String>> {
        let mut out = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = self
                .on_primary(redis::cmd("SCAN").arg(cursor).arg("MATCH").arg(pattern).arg("COUNT").arg(1000).clone())
                .await?;
            out.extend(keys);
            if next == 0 {
                break;
            }
            cursor = next;
        }
        Ok(out)
    }

    // ── scale-out coordination (plaintext, outside the barrier) ─────────────

    fn ops_key(&self, name: &str) -> String {
        format!("{{{}}}#{name}", self.cfg.prefix)
    }

    /// Upsert this instance's heartbeat and drop entries silent for > `stale_secs`.
    pub async fn registry_beat(&self, id: &str, entry: &Json, stale_secs: i64) -> AppResult<()> {
        let key = self.ops_key("instances");
        let _: i64 = self.on_primary(redis::cmd("HSET").arg(&key).arg(id).arg(entry.to_string()).clone()).await?;
        let now = chrono::Utc::now().timestamp();
        for (other, raw) in self.registry_raw().await? {
            let seen = serde_json::from_str::<Json>(&raw).ok().and_then(|v| v["last_seen"].as_i64()).unwrap_or(0);
            if now - seen > stale_secs {
                let _: i64 = self.on_primary(redis::cmd("HDEL").arg(&key).arg(&other).clone()).await?;
            }
        }
        Ok(())
    }

    async fn registry_raw(&self) -> AppResult<Vec<(String, String)>> {
        let flat: Vec<String> = self.on_primary(redis::cmd("HGETALL").arg(self.ops_key("instances")).clone()).await?;
        Ok(flat.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0].clone(), c[1].clone())).collect())
    }

    pub async fn registry_list(&self) -> AppResult<Vec<Json>> {
        Ok(self
            .registry_raw()
            .await?
            .into_iter()
            .filter_map(|(_, raw)| serde_json::from_str(&raw).ok())
            .collect())
    }

    pub async fn ops_get(&self, name: &str) -> AppResult<Option<String>> {
        Ok(self.on_primary(redis::cmd("GET").arg(self.ops_key(name)).clone()).await?)
    }

    pub async fn ops_set(&self, name: &str, value: &str) -> AppResult<()> {
        let _: () = self.on_primary(redis::cmd("SET").arg(self.ops_key(name)).arg(value).clone()).await?;
        Ok(())
    }

    /// `INFO <section>` of the primary, parsed into key/value pairs.
    pub async fn info(&self, section: &str) -> AppResult<BTreeMap<String, String>> {
        let raw: String = self.on_primary(redis::cmd("INFO").arg(section).clone()).await?;
        Ok(parse_info(&raw))
    }

    /// `CONFIG GET <param>` on the primary. `None` when CONFIG is disabled (common
    /// on managed Redis) rather than failing the whole request.
    pub async fn config_get(&self, param: &str) -> Option<String> {
        let pairs: Vec<String> = self.on_primary(redis::cmd("CONFIG").arg("GET").arg(param).clone()).await.ok()?;
        pairs.get(1).cloned()
    }

    /// How durable and available is Redis right now? Reads its own settings and
    /// flags the ones that can lose or evict vault data.
    pub async fn describe(&self) -> AppResult<Json> {
        let persistence = self.info("persistence").await?;
        let server = self.info("server").await?;
        let memory = self.info("memory").await?;
        let replication = self.info("replication").await?;
        let appendfsync = self.config_get("appendfsync").await;
        let save = self.config_get("save").await;
        let policy = self
            .config_get("maxmemory-policy")
            .await
            .or_else(|| memory.get("maxmemory_policy").cloned());
        let keys = self.list("").await?.len();
        let replicas: u32 = replication.get("connected_slaves").and_then(|v| v.parse().ok()).unwrap_or(0);

        let cluster = if self.cfg.mode == RedisMode::Cluster {
            let raw: String = self.on_primary(redis::cmd("CLUSTER").arg("INFO").clone()).await?;
            Some(parse_info(&raw))
        } else {
            None
        };

        let mut warnings: Vec<String> = Vec::new();
        let aof_on = persistence.get("aof_enabled").map(|v| v == "1").unwrap_or(false);
        if !aof_on {
            warnings.push("AOF is off — writes since the last RDB snapshot are lost on a crash. Run Redis with `--appendonly yes`.".into());
        }
        if appendfsync.as_deref() == Some("no") {
            warnings.push("appendfsync=no leaves flushing to the OS; use `everysec` (≤1s loss) or `always`.".into());
        }
        if let Some(p) = policy.as_deref() {
            if p != "noeviction" {
                warnings.push("maxmemory-policy is not `noeviction` — Redis may silently EVICT encrypted secrets or the keyring under memory pressure.".into());
            }
        }
        if !aof_on && save.as_deref().map(str::is_empty).unwrap_or(false) {
            warnings.push("Neither AOF nor RDB snapshots are enabled — Redis is running memory-only.".into());
        }
        if self.cfg.mode != RedisMode::Standalone && replicas == 0 {
            warnings.push(format!(
                "{} mode but the primary has no connected replicas — a failover has nothing to promote.",
                self.cfg.mode.as_str()
            ));
        }
        if replicas > 0 && self.cfg.wait_replicas == 0 {
            warnings.push("Replication is asynchronous: a failover can lose writes that were already acknowledged. Set REDIS_WAIT_REPLICAS=1.".into());
        }
        if self.cfg.wait_replicas > replicas {
            warnings.push(format!(
                "REDIS_WAIT_REPLICAS={} but only {replicas} replica(s) connected — every write will fail.",
                self.cfg.wait_replicas
            ));
        }
        if let Some(c) = &cluster {
            if c.get("cluster_state").map(String::as_str) != Some("ok") {
                warnings.push(format!("Cluster state is `{}`.", c.get("cluster_state").cloned().unwrap_or_default()));
            }
        }

        Ok(json!({
            "engine": "redis",
            "mode": self.cfg.mode.as_str(),
            "redis_version": server.get("redis_version"),
            "prefix": self.ns,
            "keys": keys,
            "aof_enabled": aof_on,
            "appendfsync": appendfsync,
            "rdb_save": save,
            "rdb_last_save": persistence.get("rdb_last_save_time").and_then(|v| v.parse::<i64>().ok()),
            "rdb_changes_since_last_save": persistence.get("rdb_changes_since_last_save").and_then(|v| v.parse::<i64>().ok()),
            "aof_last_write_status": persistence.get("aof_last_write_status"),
            "maxmemory_policy": policy,
            "used_memory_human": memory.get("used_memory_human"),
            "role": replication.get("role"),
            "replicas": replicas,
            "wait_replicas": self.cfg.wait_replicas,
            "sentinel_master": (self.cfg.mode == RedisMode::Sentinel).then(|| self.cfg.sentinel_master.clone()),
            "sentinels": (self.cfg.mode == RedisMode::Sentinel).then_some(self.cfg.sentinels.len()),
            "cluster": cluster.map(|c| json!({
                "state": c.get("cluster_state"),
                "known_nodes": c.get("cluster_known_nodes").and_then(|v| v.parse::<u32>().ok()),
                "size": c.get("cluster_size").and_then(|v| v.parse::<u32>().ok()),
                "slot": self.slot,
            })),
            "warnings": warnings,
        }))
    }
}

impl RedisConfig {
    /// (connect, response). The response timeout must outlast a WAIT.
    fn timeouts(&self) -> (Duration, Duration) {
        let t = Duration::from_millis(self.timeout_ms);
        (t, t.max(Duration::from_millis(self.wait_timeout_ms + 1000)))
    }

    fn async_config(&self) -> AsyncConnectionConfig {
        let (c, r) = self.timeouts();
        AsyncConnectionConfig::new().set_connection_timeout(c).set_response_timeout(r)
    }
}

fn parse_info(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}
