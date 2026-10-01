//! Audit log: an append-only record of every API request and its outcome —
//! who (token HMAC, identity), what (method, path), from where, and the result.
//!
//! Modeled on Vault's audit devices:
//! - Two entries per request sharing an `id`: `request` is written **before**
//!   the handler runs, `response` after. With `AUDIT_FAIL_CLOSED` (default when
//!   auditing is on) a request that can't be recorded by any sink is refused
//!   instead of executed unlogged.
//! - Secrets never reach the log: request/response **bodies are not recorded**
//!   (they carry secret values and unseal keys) — only a short `target` naming
//!   what was acted on (a secret *path*, a user or server name; `audit::target`),
//!   and tokens appear only as
//!   `hmac-sha256:…` keyed from the root key — identical on every instance, so
//!   `POST /v1/sys/audit-hash` can find a given token's entries.
//! - Tamper-evident: each entry carries `prev_hash` and `hash`
//!   (sha256 over the entry), chaining the whole file. Deleting, inserting or
//!   editing a line breaks the chain — `timika operator audit-verify` checks it.
//!
//! Sinks (any combination; a request proceeds if at least one succeeds):
//!   AUDIT_FILE           path; `{instance}` is replaced by this instance's id.
//!                        Rotates at AUDIT_FILE_MAX_MB (100) keeping AUDIT_FILE_MAX_FILES (10).
//!   AUDIT_FILE_FSYNC     `true` = fsync every entry (slower, survives power loss)
//!   AUDIT_SYSLOG         `udp://host:514` or `tcp://host:601` (RFC 5424, facility authpriv)
//!   AUDIT_STDOUT         `true` = also print entries to stdout
//!   AUDIT_FAIL_CLOSED    default `true` when any sink is configured

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::state::AppState;
use crate::token::TokenEntry;

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
pub const REQUEST_ID: &str = "x-timika-request-id";

/// Attached to error responses so the audit entry records why a request failed.
#[derive(Clone)]
pub struct AuditError(pub String);

tokio::task_local! {
    static TARGET: std::cell::RefCell<Option<String>>;
}

/// Name what the current request acts on (a secret path, a user, a server…),
/// for the audit trail. Never pass a secret value. No-op outside a request.
pub fn target(t: impl Into<String>) {
    let mut t: String = t.into();
    // Callers control these strings: keep entries bounded.
    if t.chars().count() > 200 {
        t = t.chars().take(200).chain("…".chars()).collect();
    }
    let _ = TARGET.try_with(|c| *c.borrow_mut() = Some(t));
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

// ─── sinks ───────────────────────────────────────────────────────────────────

struct FileSink {
    path: PathBuf,
    file: File,
    size: u64,
    max_bytes: u64,
    max_files: usize,
    fsync: bool,
}

impl FileSink {
    fn open(path: PathBuf, max_bytes: u64, max_files: usize, fsync: bool) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        Ok(Self { path, file, size, max_bytes, max_files, fsync })
    }

    fn rotated(&self, n: usize) -> PathBuf {
        PathBuf::from(format!("{}.{n}", self.path.display()))
    }

    /// audit.log → audit.log.1 → … → audit.log.N (oldest deleted). The hash
    /// chain continues into the new file, so rotated files verify together.
    fn rotate(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        let _ = std::fs::remove_file(self.rotated(self.max_files));
        for n in (1..self.max_files).rev() {
            let from = self.rotated(n);
            if from.exists() {
                std::fs::rename(&from, self.rotated(n + 1))?;
            }
        }
        std::fs::rename(&self.path, self.rotated(1))?;
        self.file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.size = 0;
        Ok(())
    }

    fn write(&mut self, line: &str) -> std::io::Result<()> {
        if self.size > 0 && self.size + line.len() as u64 + 1 > self.max_bytes {
            self.rotate()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        if self.fsync {
            self.file.sync_data()?;
        }
        self.size += line.len() as u64 + 1;
        Ok(())
    }

    /// Last entry's (hash, seq), to resume the chain after a restart.
    fn tail(path: &Path) -> Option<(String, u64)> {
        let f = File::open(path).ok()?;
        let last = BufReader::new(f).lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).last()?;
        let v: Value = serde_json::from_str(&last).ok()?;
        Some((v["hash"].as_str()?.to_string(), v["seq"].as_u64()?))
    }
}

enum SyslogTransport {
    Udp(tokio::net::UdpSocket, String),
    Tcp(String, Option<tokio::net::TcpStream>),
}

struct SyslogSink {
    transport: SyslogTransport,
    host: String,
}

impl SyslogSink {
    async fn open(url: &str, host: &str) -> anyhow::Result<Self> {
        let transport = if let Some(addr) = url.strip_prefix("udp://") {
            let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
            SyslogTransport::Udp(sock, addr.to_string())
        } else if let Some(addr) = url.strip_prefix("tcp://") {
            SyslogTransport::Tcp(addr.to_string(), None)
        } else {
            anyhow::bail!("AUDIT_SYSLOG must be udp://host:port or tcp://host:port");
        };
        Ok(Self { transport, host: host.to_string() })
    }

    async fn write(&mut self, line: &str) -> std::io::Result<()> {
        // RFC 5424, facility authpriv (10), severity info (6) → PRI 86.
        let msg = format!(
            "<86>1 {} {} timika {} audit - {line}",
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            self.host,
            std::process::id()
        );
        match &mut self.transport {
            SyslogTransport::Udp(sock, addr) => {
                sock.send_to(msg.as_bytes(), addr.as_str()).await?;
            }
            SyslogTransport::Tcp(addr, stream) => {
                // RFC 6587 octet-counting framing; reconnect once on failure.
                let framed = format!("{} {msg}", msg.len());
                for attempt in 0..2 {
                    if stream.is_none() {
                        *stream = Some(tokio::net::TcpStream::connect(addr.as_str()).await?);
                    }
                    match stream.as_mut().unwrap().write_all(framed.as_bytes()).await {
                        Ok(()) => break,
                        Err(e) if attempt == 0 => {
                            tracing::warn!("audit syslog: {e}; reconnecting");
                            *stream = None;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Ok(())
    }
}

enum Sink {
    File(FileSink),
    Syslog(SyslogSink),
    Stdout,
}

impl Sink {
    fn name(&self) -> String {
        match self {
            Sink::File(f) => format!("file:{}", f.path.display()),
            Sink::Syslog(s) => match &s.transport {
                SyslogTransport::Udp(_, a) => format!("syslog:udp://{a}"),
                SyslogTransport::Tcp(a, _) => format!("syslog:tcp://{a}"),
            },
            Sink::Stdout => "stdout".into(),
        }
    }
}

// ─── auditor ─────────────────────────────────────────────────────────────────

struct Chain {
    sinks: Vec<Sink>,
    prev: String,
    seq: u64,
}

pub struct Auditor {
    instance: String,
    /// The local audit file, if any (what the UI's audit trail reads).
    file: Option<PathBuf>,
    fail_closed: bool,
    chain: Mutex<Chain>,
}

impl Auditor {
    pub async fn from_env(instance: &str) -> anyhow::Result<Option<Arc<Self>>> {
        let mut sinks = Vec::new();
        let mut file = None;
        let (mut prev, mut seq) = (GENESIS.to_string(), 0);
        if let Some(path) = env("AUDIT_FILE") {
            let path = PathBuf::from(path.replace("{instance}", &crate::logging::sanitize(instance)));
            if let Some((h, s)) = FileSink::tail(&path) {
                (prev, seq) = (h, s);
            }
            let max_mb: u64 = env("AUDIT_FILE_MAX_MB").and_then(|v| v.parse().ok()).unwrap_or(100);
            let max_files: usize = env("AUDIT_FILE_MAX_FILES").and_then(|v| v.parse().ok()).unwrap_or(10).max(1);
            let fsync = env("AUDIT_FILE_FSYNC").as_deref() == Some("true");
            file = Some(path.clone());
            sinks.push(Sink::File(FileSink::open(path, max_mb * 1024 * 1024, max_files, fsync)?));
        }
        if let Some(url) = env("AUDIT_SYSLOG") {
            sinks.push(Sink::Syslog(SyslogSink::open(&url, instance).await?));
        }
        if env("AUDIT_STDOUT").as_deref() == Some("true") {
            sinks.push(Sink::Stdout);
        }
        if sinks.is_empty() {
            tracing::warn!("audit log is OFF — set AUDIT_FILE and/or AUDIT_SYSLOG (docs/LOGGING.md)");
            return Ok(None);
        }
        let fail_closed = env("AUDIT_FAIL_CLOSED").map(|v| v != "false").unwrap_or(true);
        tracing::info!(
            "audit log → {} (fail-closed: {fail_closed}, chain resumes at seq {seq})",
            sinks.iter().map(Sink::name).collect::<Vec<_>>().join(", ")
        );
        Ok(Some(Arc::new(Self { instance: instance.to_string(), file, fail_closed, chain: Mutex::new(Chain { sinks, prev, seq }) })))
    }

    pub async fn describe(&self) -> Value {
        let c = self.chain.lock().await;
        json!({
            "enabled": true,
            "sinks": c.sinks.iter().map(Sink::name).collect::<Vec<_>>(),
            "fail_closed": self.fail_closed,
            "seq": c.seq,
            "last_hash": c.prev,
        })
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// This instance's audit files, oldest first (None = no local file sink).
    pub fn files(&self) -> Option<Vec<PathBuf>> {
        self.file.as_deref().map(with_rotated)
    }

    /// Record an event that isn't a request (e.g. a bastion session ending).
    pub async fn record(&self, entry: Map<String, Value>) {
        if let Err(e) = self.log(entry).await {
            tracing::error!("audit: could not record event: {e}");
        }
    }

    /// Chain and write one entry. Ok if at least one sink recorded it.
    async fn log(&self, mut entry: Map<String, Value>) -> Result<(), String> {
        let mut c = self.chain.lock().await;
        let seq = c.seq + 1;
        entry.insert("instance".into(), json!(self.instance));
        entry.insert("seq".into(), json!(seq));
        entry.insert("prev_hash".into(), json!(c.prev));
        let hash = entry_hash(&entry);
        entry.insert("hash".into(), json!(hash));
        let line = Value::Object(entry).to_string();

        let mut ok = 0;
        let mut errors = Vec::new();
        for sink in c.sinks.iter_mut() {
            let res = match sink {
                Sink::File(f) => f.write(&line),
                Sink::Syslog(s) => s.write(&line).await,
                Sink::Stdout => {
                    println!("{line}");
                    Ok(())
                }
            };
            match res {
                Ok(()) => ok += 1,
                Err(e) => errors.push(format!("{}: {e}", sink.name())),
            }
        }
        if !errors.is_empty() {
            tracing::error!("audit sink failure: {}", errors.join("; "));
        }
        if ok == 0 {
            return Err(errors.join("; "));
        }
        // Only advance the chain once something durable holds this entry.
        c.prev = hash;
        c.seq = seq;
        Ok(())
    }
}

/// sha256 over the entry without its `hash` field. serde_json maps are sorted,
/// so the serialization — and the hash — is deterministic.
fn entry_hash(entry: &Map<String, Value>) -> String {
    let mut e = entry.clone();
    e.remove("hash");
    hex::encode(Sha256::digest(Value::Object(e).to_string().as_bytes()))
}

// ─── middleware ──────────────────────────────────────────────────────────────

/// High-frequency, unauthenticated polling (load balancers, probes, the UI) —
/// auditing it would bury the real events.
fn skipped(method: &str, path: &str) -> bool {
    (method == "GET" && path == "/v1/sys/health")
        || matches!(
            path,
            "/timika.v1.SysService/Health"
                | "/timika.v1.SysService/GetSealStatus"
                | "/timika.v1.SysService/Instances"
                | "/timika.v1.AuthService/GetLoginConfig"
                | "/timika.v1.AuthService/GetCaptchaChallenge"
        )
}

/// gRPC / gRPC-Web calls (`/timika.v1.<Service>/<Method>`).
fn is_grpc(path: &str) -> bool {
    path.starts_with("/timika.v1.")
}

/// The fail-closed refusal, in the shape the caller understands.
fn refusal(grpc_content_type: Option<HeaderValue>) -> Response {
    const MSG: &str = "audit log unavailable — request refused (fail-closed)";
    match grpc_content_type {
        Some(ct) => {
            let mut r = tonic::Status::unavailable(MSG).into_http().map(axum::body::Body::new);
            // gRPC-Web clients need their own content type back.
            r.headers_mut().insert(axum::http::header::CONTENT_TYPE, ct);
            r.into_response()
        }
        None => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": MSG }))).into_response(),
    }
}

fn token_of(req: &Request) -> Option<String> {
    let h = req.headers();
    h.get("x-timika-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(|s| s.trim().to_string())
        })
        .or_else(|| ws_token(h))
        .filter(|t| !t.is_empty())
}

/// Browsers can't set headers on WebSockets, so the web terminal sends its
/// token as a subprotocol (`Sec-WebSocket-Protocol: timika, tmk.…`).
pub fn ws_token(h: &axum::http::HeaderMap) -> Option<String> {
    h.get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').map(str::trim).find(|p| p.starts_with("tmk.")).map(String::from))
}

fn random_id() -> String {
    let mut b = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    hex::encode(b)
}

pub async fn middleware(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(aud) = st.audit.clone() else { return next.run(req).await };
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let grpc = is_grpc(&path);
    if !(grpc || path.starts_with("/v1/")) || skipped(&method, &path) {
        return next.run(req).await;
    }

    let headers = req.headers();
    let id = random_id();
    let grpc_ct = grpc.then(|| headers.get(axum::http::header::CONTENT_TYPE).cloned()).flatten();
    let remote = req.extensions().get::<ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip().to_string());
    let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).map(String::from);
    let token_hmac = match token_of(&req) {
        Some(t) => Some(st.core.audit_hmac(t.as_bytes()).await.unwrap_or_else(|| "unavailable-while-sealed".into())),
        None => None,
    };
    let request = if grpc {
        let web = grpc_ct.as_ref().and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("application/grpc-web"));
        json!({ "rpc": path.trim_start_matches('/'), "protocol": if web { "grpc-web" } else { "grpc" } })
    } else {
        json!({ "method": method, "path": path, "query": req.uri().query() })
    };

    let mut e = Map::new();
    e.insert("type".into(), json!("request"));
    e.insert("time".into(), json!(Utc::now()));
    e.insert("id".into(), json!(id));
    e.insert("remote_addr".into(), json!(remote));
    if let Some(x) = &xff {
        e.insert("x_forwarded_for".into(), json!(x));
    }
    e.insert("auth".into(), json!({ "token_hmac": token_hmac }));
    e.insert("request".into(), request.clone());

    if let Err(err) = aud.log(e).await {
        if aud.fail_closed {
            tracing::error!("audit: refusing {method} {path} — no sink could record it ({err})");
            return refusal(grpc.then(|| grpc_ct.clone().unwrap_or(HeaderValue::from_static("application/grpc"))));
        }
    }

    if let Ok(v) = HeaderValue::from_str(&id) {
        req.headers_mut().insert(REQUEST_ID, v);
    }
    let started = Instant::now();
    let (mut resp, target) = TARGET
        .scope(std::cell::RefCell::new(None), async {
            let resp = next.run(req).await;
            (resp, TARGET.with(|c| c.borrow().clone()))
        })
        .await;

    let who = resp.extensions().get::<TokenEntry>().cloned();
    let mut error = resp.extensions().get::<AuditError>().map(|e| e.0.clone());
    // gRPC errors are trailers-only responses: the status is a header. No
    // header = OK (it's in the trailers of a successful body).
    let grpc_status = grpc.then(|| {
        let h = resp.headers();
        if error.is_none() {
            error = h
                .get("grpc-message")
                .and_then(|v| v.to_str().ok())
                .map(|m| percent_decode(m).chars().take(300).collect());
        }
        h.get("grpc-status").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0)
    });
    let mut e = Map::new();
    e.insert("type".into(), json!("response"));
    e.insert("time".into(), json!(Utc::now()));
    e.insert("id".into(), json!(id));
    e.insert(
        "auth".into(),
        json!({
            "token_hmac": token_hmac,
            "display_name": who.as_ref().map(|w| w.display_name.clone()),
            "policies": who.as_ref().map(|w| w.policies.clone()),
        }),
    );
    e.insert("request".into(), request);
    if let Some(t) = &target {
        e.insert("target".into(), json!(t));
    }
    e.insert(
        "response".into(),
        match grpc_status {
            Some(code) => json!({
                "grpc_status": code,
                "error": error,
                "duration_ms": started.elapsed().as_millis() as u64,
            }),
            None => json!({
                "status": resp.status().as_u16(),
                "error": error,
                "duration_ms": started.elapsed().as_millis() as u64,
            }),
        },
    );
    if let Err(err) = aud.log(e).await {
        tracing::error!("audit: could not record the response of {id}: {err}");
    }
    if let Ok(v) = HeaderValue::from_str(&id) {
        resp.headers_mut().insert(REQUEST_ID, v);
    }
    resp
}

/// `grpc-message` is percent-encoded.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ─── reading the trail back ──────────────────────────────────────────────────

/// One audited call (request + response joined) or a session event, as the UI
/// shows it.
#[derive(Debug, Clone, Default)]
pub struct Event {
    pub seq: u64,
    pub time: String,
    pub id: String,
    pub kind: &'static str,
    pub action: String,
    pub user: Option<String>,
    pub policies: Vec<String>,
    pub remote_addr: Option<String>,
    pub target: Option<String>,
    pub ok: bool,
    pub code: i32,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub details: Value,
    pub instance: String,
}

#[derive(Debug, Default)]
pub struct Filter {
    pub user: String,
    pub query: String,
    /// "", "ok", "error"
    pub outcome: String,
    /// "", "secrets", "signin", "users", "servers", "sessions", "system"
    pub category: String,
    pub before_seq: Option<u64>,
    pub include_routine: bool,
    pub server: String,
}

/// Whether an event's target is about bastion server `id`. Targets are written
/// as `server <id>…`, `<account>@<id>`, `… → server <id>…` or `… on server <id>`.
pub fn about_server(target: &str, id: &str) -> bool {
    let word_end = |rest: &str| rest.is_empty() || rest.starts_with(' ') || rest.starts_with('(');
    if let Some(rest) = target.strip_prefix(&format!("server {id}")) {
        return word_end(rest);
    }
    // `<account>@<id>` or `<account>@<id>:<path> (…)` (terminal, files).
    let first = target.split(' ').next().unwrap_or("");
    if let Some((_, rest)) = first.split_once('@') {
        if rest.split(':').next() == Some(id) && (target.contains(':') || !target.contains(' ')) {
            return true;
        }
    }
    for marker in ["→ server ", "on server "] {
        if let Some(rest) = target.split_once(marker).map(|(_, r)| r) {
            if let Some(after) = rest.strip_prefix(id) {
                if word_end(after) {
                    return true;
                }
            }
        }
    }
    false
}

/// Reads the UI repeats on its own (polling, session keep-alive, list views):
/// recorded, but hidden from the trail unless asked for.
pub fn routine(action: &str) -> bool {
    matches!(
        action,
        "AuthService/LookupSelf"
            | "AuthService/RenewSelf"
            | "AuthService/ListUsers"
            | "AuditService/ListEvents"
            | "SysService/GetKeyStatus"
            | "SysService/Storage"
            | "SysService/Audit"
            | "BastionService/ListAssets"
            | "BastionService/ListSessions"
            | "BastionService/ListGrants"
            | "BastionService/ListAllGrants"
            | "BastionService/ListCommands"
            | "BastionService/ListFiles"
            | "BastionService/DownloadLink"
            | "BastionService/ArchiveLink"
            | "AutomationService/ListRepos"
            | "AutomationService/GetRepo"
            | "AutomationService/ListRuns"
            | "AutomationService/GetRun"
            | "AutomationService/WatchRun"
            | "AutomationService/ListSchedules"
            | "AutomationService/ListRepoFiles"
            | "ClusterService/Configuration"
            | "KvService/List"
            | "KvService/GetMetadata"
    )
}

/// Which UI category an action belongs to.
pub fn category(action: &str) -> &'static str {
    let rpc = action.split('/').nth(1).unwrap_or_default();
    if action.contains("/v1/bastion/files/") || matches!(rpc, "ListFiles" | "MakeDir" | "RenameFile" | "DeleteFiles" | "DownloadLink" | "Compress" | "Extract" | "ArchiveLink") {
        "files"
    } else if action.starts_with("session") || action.contains("/v1/bastion/connect") || rpc.contains("Session") || rpc == "GetRecording" {
        "sessions"
    } else if action.starts_with("AutomationService/") || action.contains("/v1/automation/") {
        "automation"
    } else if action.starts_with("KvService/") {
        "secrets"
    } else if matches!(rpc, "Login" | "LookupSelf" | "RenewSelf" | "RevokeSelf" | "ChangePassword") {
        "signin"
    } else if action.starts_with("AuthService/") {
        "users"
    } else if action.starts_with("BastionService/") {
        "servers"
    } else {
        "system"
    }
}

fn to_event(e: &Value) -> Option<Event> {
    let s = |v: &Value| v.as_str().map(String::from);
    let base = Event {
        seq: e["seq"].as_u64()?,
        time: s(&e["time"])?,
        id: s(&e["id"]).unwrap_or_default(),
        instance: s(&e["instance"]).unwrap_or_default(),
        user: s(&e["auth"]["display_name"]),
        policies: e["auth"]["policies"].as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).unwrap_or_default(),
        target: s(&e["target"]),
        ..Default::default()
    };
    match e["type"].as_str()? {
        "response" => {
            let r = &e["response"];
            let (action, code, ok) = match (s(&e["request"]["rpc"]), r["grpc_status"].as_i64()) {
                (Some(rpc), Some(c)) => (rpc.trim_start_matches("timika.v1.").to_string(), c as i32, c == 0),
                _ => {
                    let st = r["status"].as_i64().unwrap_or(0) as i32;
                    let m = s(&e["request"]["method"]).unwrap_or_default();
                    let p = s(&e["request"]["path"]).unwrap_or_default();
                    (format!("{m} {p}"), st, (200..400).contains(&st) || st == 101)
                }
            };
            // The terminal's target is in its query (server + account).
            let target = base.target.clone().or_else(|| {
                let q = e["request"]["query"].as_str()?;
                let get = |k: &str| q.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).map(String::from);
                Some(format!("{}@{}", get("account")?, get("asset")?))
            });
            Some(Event {
                kind: "call",
                action,
                ok,
                code,
                error: s(&r["error"]),
                duration_ms: r["duration_ms"].as_u64().unwrap_or(0),
                target,
                ..base
            })
        }
        "bastion-session" => {
            let d = &e["session"];
            let status = d["status"].as_str().unwrap_or("?");
            Some(Event {
                kind: "session",
                action: format!("session {status}"),
                ok: !matches!(status, "failed"),
                error: s(&d["error"]),
                target: Some(format!("{}@{}", d["account"].as_str().unwrap_or("?"), d["asset"].as_str().unwrap_or("?"))),
                remote_addr: s(&d["client_ip"]),
                details: d.clone(),
                ..base
            })
        }
        _ => None,
    }
}

fn matches(ev: &Event, f: &Filter) -> bool {
    if f.before_seq.is_some_and(|b| ev.seq >= b) {
        return false;
    }
    // Routine reads are hidden — but never a failed one (that's worth seeing).
    if !f.include_routine && ev.ok && routine(&ev.action) {
        return false;
    }
    if !f.user.is_empty() && !ev.user.as_deref().is_some_and(|u| u.eq_ignore_ascii_case(f.user.trim())) {
        return false;
    }
    match f.outcome.as_str() {
        "ok" if !ev.ok => return false,
        "error" if ev.ok => return false,
        _ => {}
    }
    if !f.category.is_empty() && category(&ev.action) != f.category {
        return false;
    }
    if !f.server.is_empty() && !ev.target.as_deref().is_some_and(|t| about_server(t, &f.server)) {
        return false;
    }
    let q = f.query.trim().to_lowercase();
    if !q.is_empty() {
        let hay = [ev.action.as_str(), ev.target.as_deref().unwrap_or(""), ev.user.as_deref().unwrap_or(""), ev.remote_addr.as_deref().unwrap_or(""), ev.error.as_deref().unwrap_or("")]
            .join(" ")
            .to_lowercase();
        if !hay.contains(&q) {
            return false;
        }
    }
    true
}

/// Newest-first events from the audit files (oldest first, as `with_rotated`
/// returns them). Returns the page and, if there may be more, the `seq` to
/// continue before.
pub fn read_events(files: &[PathBuf], f: &Filter, limit: usize) -> (Vec<Event>, Option<u64>) {
    let mut out: Vec<Event> = Vec::new();
    // Responses seen (newest first) still waiting for their request's address.
    let mut pending: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut after_full = 0usize;
    'files: for file in files.iter().rev() {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        for line in text.lines().rev() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["type"] == "request" {
                if let Some(i) = v["id"].as_str().and_then(|id| pending.remove(id)) {
                    out[i].remote_addr = v["remote_addr"].as_str().map(String::from);
                }
            } else if out.len() < limit {
                if let Some(ev) = to_event(&v) {
                    if matches(&ev, f) {
                        if ev.kind == "call" {
                            pending.insert(ev.id.clone(), out.len());
                        }
                        out.push(ev);
                    }
                }
            }
            if out.len() >= limit {
                // Keep reading a little so the last events get their address.
                after_full += 1;
                if pending.is_empty() || after_full > 400 {
                    break 'files;
                }
            }
        }
    }
    let next = (out.len() >= limit).then(|| out.last().map(|e| e.seq)).flatten();
    (out, next)
}

// ─── verification (offline) ──────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Verification {
    pub entries: u64,
    /// Places where a fresh chain starts (first ever entry, or the file was
    /// recreated). Expected once per instance; more deserves a look.
    pub chain_starts: Vec<String>,
    /// Hash mismatches or broken links — evidence of tampering or loss.
    pub problems: Vec<String>,
}

/// Verify files in chronological order (oldest rotated file first).
pub fn verify(files: &[PathBuf]) -> anyhow::Result<Verification> {
    let mut v = Verification::default();
    let mut prev: Option<String> = None;
    for f in files {
        let reader = BufReader::new(File::open(f)?);
        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let at = format!("{}:{}", f.display(), i + 1);
            let entry: Map<String, Value> = match serde_json::from_str::<Value>(&line) {
                Ok(Value::Object(m)) => m,
                _ => {
                    v.problems.push(format!("{at}: not a JSON object"));
                    continue;
                }
            };
            v.entries += 1;
            let stated = entry.get("hash").and_then(Value::as_str).unwrap_or_default().to_string();
            if entry_hash(&entry) != stated {
                v.problems.push(format!("{at}: content does not match its hash (entry was modified)"));
            }
            let link = entry.get("prev_hash").and_then(Value::as_str).unwrap_or_default();
            match &prev {
                _ if link == GENESIS => v.chain_starts.push(at.clone()),
                Some(p) if p != link => v.problems.push(format!("{at}: prev_hash does not match the previous entry (entries or files removed, or reordered)")),
                None => v.problems.push(format!("{at}: first entry links to a hash that is not in these files (earlier files missing?)")),
                _ => {}
            }
            prev = Some(stated);
        }
    }
    Ok(v)
}

/// `audit.log` → [audit.log.N … audit.log.1, audit.log] (oldest first): every
/// rotated sibling present, so a missing file in the middle shows up as a
/// broken link rather than silently shortening the check.
pub fn with_rotated(base: &Path) -> Vec<PathBuf> {
    let dir = base.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = base.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut rotated: Vec<(usize, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| {
                    let f = e.file_name().to_string_lossy().to_string();
                    let n = f.strip_prefix(&format!("{name}."))?.parse::<usize>().ok()?;
                    Some((n, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    rotated.sort_by_key(|r| std::cmp::Reverse(r.0));
    let mut out: Vec<PathBuf> = rotated.into_iter().map(|(_, p)| p).collect();
    out.push(base.to_path_buf());
    out
}

/// Response body for sys/audit when auditing is off.
pub fn disabled() -> Value {
    json!({ "enabled": false, "hint": "set AUDIT_FILE and/or AUDIT_SYSLOG" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_targets() {
        for t in ["server web-1", "server web-1 (host key reset)", "deploy@web-1", "user jane → server web-1", "user jane → server web-1 (ops)", "grant ab12 on server web-1", "deploy@web-1:/etc/my file.conf (download)", "deploy@web-1:/srv"] {
            assert!(about_server(t, "web-1"), "{t}");
        }
        for t in ["server web-10", "deploy@web-10", "user jane → server web-12", "app/db", "user web-1", "a b@web-1", "deploy@web-10:/etc"] {
            assert!(!about_server(t, "web-1"), "{t}");
        }
    }

    fn chain(n: usize) -> Vec<String> {
        let mut prev = GENESIS.to_string();
        (0..n)
            .map(|i| {
                let mut e = Map::new();
                e.insert("seq".into(), json!(i + 1));
                e.insert("request".into(), json!({ "path": format!("/v1/kv/data/p{i}") }));
                e.insert("prev_hash".into(), json!(prev));
                let h = entry_hash(&e);
                e.insert("hash".into(), json!(h));
                prev = h;
                Value::Object(e).to_string()
            })
            .collect()
    }

    fn check(lines: &[String]) -> Verification {
        let dir = std::env::temp_dir().join(format!("timika-audit-test-{}", random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("audit.log");
        std::fs::write(&f, lines.join("\n") + "\n").unwrap();
        let v = verify(&[f]).unwrap();
        std::fs::remove_dir_all(dir).ok();
        v
    }

    #[test]
    fn intact_chain_verifies() {
        let v = check(&chain(5));
        assert_eq!(v.entries, 5);
        assert!(v.problems.is_empty(), "{:?}", v.problems);
        assert_eq!(v.chain_starts.len(), 1);
    }

    #[test]
    fn edit_is_detected() {
        let mut lines = chain(5);
        lines[2] = lines[2].replace("/v1/kv/data/p2", "/v1/kv/data/px");
        let v = check(&lines);
        assert!(v.problems.iter().any(|p| p.contains("modified")), "{:?}", v.problems);
    }

    #[test]
    fn deletion_is_detected() {
        let mut lines = chain(5);
        lines.remove(2);
        let v = check(&lines);
        assert!(v.problems.iter().any(|p| p.contains("removed")), "{:?}", v.problems);
    }
}
