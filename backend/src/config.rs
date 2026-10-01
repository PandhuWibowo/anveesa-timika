use std::path::PathBuf;

/// Env-driven configuration. Everything has a local-dev default so
/// `cargo run` works against `make redis` with no .env at all.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: String,
    /// `redis` (default) or `raft`.
    pub storage: StorageKind,
    pub kv_max_versions: u32,

    // ── redis ──
    pub redis: RedisConfig,

    // ── raft ──
    /// Directory holding this node's redb file.
    pub raft_path: PathBuf,
    /// Node name — unique and stable per node (the pod name on Kubernetes).
    /// Also this instance's id in the Redis instance registry.
    pub node_name: String,
    /// This node's API address as peers and forwarded requests should reach it.
    pub api_addr: String,
    /// This node's Raft RPC address as peers should reach it.
    pub cluster_addr: String,
    pub cluster_bind_addr: String,
    /// Peer API addresses to try joining through.
    pub retry_join: Vec<String>,
    /// Snapshot after this many log entries (Vault's `snapshot_threshold`).
    pub snapshot_threshold: u64,
    /// Log entries kept after a snapshot so lagging peers can catch up without
    /// a full snapshot transfer (Vault's `trailing_logs`).
    pub trailing_logs: u64,

    // ── tls (both listeners + outbound peer calls) ──
    pub tls_cert_file: Option<PathBuf>,
    pub tls_key_file: Option<PathBuf>,
    /// CA that signed the peers' certs (outbound trust).
    pub tls_ca_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisMode {
    Standalone,
    Sentinel,
    Cluster,
}

impl RedisMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RedisMode::Standalone => "standalone",
            RedisMode::Sentinel => "sentinel",
            RedisMode::Cluster => "cluster",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RedisConfig {
    pub mode: RedisMode,
    /// standalone: `redis://` or `rediss://` URL.
    pub url: String,
    /// sentinel: sentinel addresses (`redis://host:26379`, auth/TLS in the URL).
    pub sentinels: Vec<String>,
    /// sentinel: the monitored primary's name.
    pub sentinel_master: String,
    /// cluster: seed nodes (`redis://host:6379` / `rediss://…`).
    pub cluster_nodes: Vec<String>,
    /// Credentials for the data nodes (sentinel + cluster; standalone uses the URL).
    pub username: Option<String>,
    pub password: Option<String>,
    /// sentinel: talk TLS to the data nodes it hands out.
    pub tls: bool,
    /// After each commit, `WAIT` until this many replicas have the write (0 = don't wait).
    pub wait_replicas: u32,
    pub wait_timeout_ms: u64,
    /// Connect/response timeout. Without it a vanished primary (host down, IP
    /// gone) stalls requests until the OS gives up on TCP — minutes, not seconds.
    pub timeout_ms: u64,
    /// Namespace for every key: `{prefix}:<path>` (the braces are a Cluster hash tag).
    pub prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    Redis,
    Raft,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let storage = match env_or("STORAGE", "redis").to_ascii_lowercase().as_str() {
            "redis" => StorageKind::Redis,
            "raft" => StorageKind::Raft,
            other => anyhow::bail!("STORAGE must be `redis` or `raft`, got `{other}`"),
        };
        let tls_cert_file = env_opt("TLS_CERT_FILE").map(PathBuf::from);
        let scheme = if tls_cert_file.is_some() { "https" } else { "http" };
        let bind_addr = env_or("BIND_ADDR", "127.0.0.1:8200");
        let cluster_bind_addr = env_or("CLUSTER_BIND_ADDR", "127.0.0.1:8201");
        let node_name = env_opt("RAFT_NODE_ID")
            .or_else(|| env_opt("INSTANCE_ID"))
            .or_else(|| env_opt("HOSTNAME"))
            .or_else(|| {
                let out = std::process::Command::new("hostname").output().ok()?;
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|h| !h.is_empty())
            })
            .unwrap_or_else(|| "timika-0".into());

        Ok(Self {
            api_addr: env_opt("API_ADDR").unwrap_or_else(|| format!("{scheme}://{}", advertise(&bind_addr))),
            cluster_addr: env_opt("CLUSTER_ADDR")
                .unwrap_or_else(|| format!("{scheme}://{}", advertise(&cluster_bind_addr))),
            bind_addr,
            cluster_bind_addr,
            storage,
            kv_max_versions: env_or("KV_MAX_VERSIONS", "10").parse().unwrap_or(10).max(1),
            redis: RedisConfig {
                mode: match env_or("REDIS_MODE", "standalone").to_ascii_lowercase().as_str() {
                    "standalone" => RedisMode::Standalone,
                    "sentinel" => RedisMode::Sentinel,
                    "cluster" => RedisMode::Cluster,
                    other => anyhow::bail!("REDIS_MODE must be standalone, sentinel or cluster, got `{other}`"),
                },
                url: env_or("REDIS_URL", "redis://127.0.0.1:6380"),
                sentinels: list("REDIS_SENTINELS", "redis://"),
                sentinel_master: env_or("REDIS_SENTINEL_MASTER", "mymaster"),
                cluster_nodes: list("REDIS_CLUSTER_NODES", "redis://"),
                username: env_opt("REDIS_USERNAME"),
                password: env_opt("REDIS_PASSWORD"),
                tls: env_or("REDIS_TLS", "false") == "true",
                wait_replicas: env_or("REDIS_WAIT_REPLICAS", "0").parse().unwrap_or(0),
                wait_timeout_ms: env_or("REDIS_WAIT_TIMEOUT_MS", "1000").parse().unwrap_or(1000),
                timeout_ms: env_or("REDIS_TIMEOUT_MS", "3000").parse().unwrap_or(3000),
                prefix: env_or("TIMIKA_PREFIX", "timika"),
            },
            raft_path: PathBuf::from(env_or("RAFT_PATH", "./data/raft")),
            node_name,
            retry_join: env_opt("RAFT_RETRY_JOIN")
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().trim_end_matches('/').to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            snapshot_threshold: env_or("RAFT_SNAPSHOT_THRESHOLD", "8192").parse().unwrap_or(8192).max(1),
            trailing_logs: env_or("RAFT_TRAILING_LOGS", "10000").parse().unwrap_or(10000),
            tls_cert_file,
            tls_key_file: env_opt("TLS_KEY_FILE").map(PathBuf::from),
            tls_ca_file: env_opt("TLS_CA_FILE").map(PathBuf::from),
        })
    }
}

/// `0.0.0.0:8200` isn't an address anyone can reach. Advertise this machine's
/// outbound IP instead (the container / pod IP when scaled out), so replicas and
/// peers can find each other without per-instance config.
fn advertise(bind: &str) -> String {
    if !bind.starts_with("0.0.0.0:") {
        return bind.to_string();
    }
    let ip = local_ip().unwrap_or_else(|| "127.0.0.1".into());
    bind.replacen("0.0.0.0", &ip, 1)
}

/// The IP the OS would use for outbound traffic. A UDP "connect" sends nothing.
fn local_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("10.255.255.255:1").ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}

/// Comma-separated list; bare `host:port` entries get `scheme` prepended.
fn list(key: &str, scheme: &str) -> Vec<String> {
    env_opt(key)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| if s.contains("://") { s.to_string() } else { format!("{scheme}{s}") })
                .collect()
        })
        .unwrap_or_default()
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_or(key: &str, default: &str) -> String {
    env_opt(key).unwrap_or_else(|| default.to_string())
}
