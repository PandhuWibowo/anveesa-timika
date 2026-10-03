//! Sources (docs/SOURCES.md): Kubernetes clusters and cloud accounts whose
//! inventory — VMs, load balancers, NAT gateways, nodes, ingresses, services,
//! workloads — joins the map. Read-only, two ways in: a CLI on a monitored
//! server (`server`), or credentials kept in the vault and the provider's API
//! called from timika (`vault`). Both give the same JSON to one parser per
//! provider, and every provider is translated into one `Inventory`.

pub mod aws;
pub mod azure;
pub mod fetch;
pub mod gcp;
pub mod kube;
pub mod tencent;

use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::Core;
use crate::error::{AppError, AppResult};

pub const KINDS: [&str; 5] = ["kubernetes", "aws", "tencent", "gcp", "azure"];
const METHOD: &str = "monitor/map-method";

/// A cloud provider's account (read with its API key), as opposed to a cluster.
pub fn is_cloud(kind: &str) -> bool {
    matches!(kind, "aws" | "tencent" | "gcp" | "azure")
}

/// How the map is detected: `vm` (the VMs only, no cloud keys used) or `api`
/// (the cloud providers' APIs as well). Clusters count in both.
pub async fn method(core: &Core) -> AppResult<String> {
    Ok(core.get_json::<String>(METHOD).await?.filter(|m| m == "api").unwrap_or_else(|| "vm".into()))
}

pub async fn set_method(core: &Core, m: &str) -> AppResult<()> {
    if !matches!(m, "vm" | "api") {
        return Err(AppError::BadRequest(format!("unknown method `{m}` (vm · api)")));
    }
    core.commit(vec![(METHOD.into(), serde_json::to_vec(m)?)], vec![], vec![]).await
}
const PREFIX: &str = "monitor/sources/";
/// Seconds between readings of a source.
const EVERY: i64 = 300;

fn cfg_path(id: &str) -> String {
    format!("{PREFIX}cfg/{id}")
}
fn secret_path(id: &str) -> String {
    format!("{PREFIX}secret/{id}")
}
fn state_path(id: &str) -> String {
    format!("{PREFIX}state/{id}")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub id: String,
    pub name: String,
    /// kubernetes · aws · tencent · gcp · azure
    pub kind: String,
    /// server (a CLI on a monitored server) · vault (credentials here, API from timika)
    pub access: String,
    /// `server`: the monitored server whose CLI is used.
    #[serde(default)]
    pub asset: String,
    /// aws · tencent: the regions to read.
    #[serde(default)]
    pub regions: Vec<String>,
    /// gcp: the project · azure: the subscription · kubernetes (server): the kubectl context.
    #[serde(default)]
    pub scope: String,
    /// `vault`: where the API is, when it is not the provider's public endpoint
    /// (kubernetes: the API server, always).
    #[serde(default)]
    pub endpoint: String,
}

/// One thing in a cloud account or a cluster.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Item {
    /// Unique within its source: "vm:i-0abc", "lb:web", "svc:shop/api".
    pub id: String,
    /// vm · lb · nat · node · ingress · service · workload
    pub kind: String,
    pub name: String,
    /// A zone or region; a namespace.
    #[serde(default)]
    pub scope: String,
    /// Every address it answers on (private, public, cluster, pods).
    #[serde(default)]
    pub ips: Vec<String>,
    /// Other names it is known by: instance id, ARN, DNS name, provider id.
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub state: String,
    /// (port, protocol, note)
    #[serde(default)]
    pub ports: Vec<(u16, String, String)>,
    /// Shown as they are: ("Subnet", "subnet-1").
    #[serde(default)]
    pub attrs: Vec<(String, String)>,
    /// Reachable from the internet.
    #[serde(default)]
    pub public: bool,
    /// Kubernetes: a service's node ports.
    #[serde(default)]
    pub node_ports: Vec<u16>,
}

/// "This forwards to that". Ends are `id:<item id>` (this source),
/// `ref:<instance id / name>`, `ip:<address>`, `ext:<address or DNS name>`
/// (whatever answers there: a load balancer of any source, else the internet)
/// or `internet`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Link {
    pub from: String,
    pub to: String,
    /// The port on the receiving side (0 = any).
    pub port: u16,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Inventory {
    pub items: Vec<Item>,
    pub links: Vec<Link>,
    /// What could not be read, and why (a region, a call).
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Inventory {
    pub fn link(&mut self, from: impl Into<String>, to: impl Into<String>, port: u16, note: impl Into<String>) {
        let l = Link { from: from.into(), to: to.into(), port, note: note.into() };
        if !self.links.contains(&l) && self.links.len() < 5000 {
            self.links.push(l);
        }
    }
    pub fn extend(&mut self, other: Inventory) {
        self.items.extend(other.items);
        self.links.extend(other.links);
        self.notes.extend(other.notes);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub inventory: Inventory,
}

// ── small JSON helpers shared by the parsers ────────────────────────────────

pub fn s(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(x) => x.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}
/// The elements of `v[k]`; a single object counts as one (XML turned into JSON).
pub fn list<'a>(v: &'a Value, k: &str) -> Vec<&'a Value> {
    match &v[k] {
        Value::Array(a) => a.iter().collect(),
        Value::Null => Vec::new(),
        Value::String(x) if x.is_empty() => Vec::new(),
        other => vec![other],
    }
}
pub fn num(v: &Value, k: &str) -> u16 {
    match &v[k] {
        Value::Number(n) => n.as_u64().and_then(|x| u16::try_from(x).ok()).unwrap_or(0),
        Value::String(x) => x.parse().unwrap_or(0),
        _ => 0,
    }
}
pub fn push(list: &mut Vec<String>, v: impl Into<String>) {
    let v = v.into();
    if !v.is_empty() && !list.contains(&v) {
        list.push(v);
    }
}
/// The last part of a URL or resource id: ".../zones/us-central1-a" → "us-central1-a".
pub fn tail(s: &str) -> &str {
    s.trim_end_matches('/').rsplit('/').next().unwrap_or(s)
}

// ── configuration ───────────────────────────────────────────────────────────

fn word(v: &str, what: &str, max: usize) -> AppResult<()> {
    let ok = !v.is_empty() && v.len() <= max && !v.starts_with('-') && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@' | '/'));
    if ok { Ok(()) } else { Err(AppError::BadRequest(format!("{what} may only hold letters, digits and - _ . : @ /"))) }
}

/// The secret fields a kind needs in the vault.
pub fn secret_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "kubernetes" => &["token", "ca"],
        "aws" => &["access_key_id", "secret_access_key", "session_token"],
        "tencent" => &["secret_id", "secret_key"],
        "gcp" => &["service_account"],
        "azure" => &["tenant_id", "client_id", "client_secret"],
        _ => &[],
    }
}
fn optional(field: &str) -> bool {
    matches!(field, "ca" | "session_token")
}

pub fn validate(src: &Source) -> AppResult<()> {
    if src.name.trim().is_empty() || src.name.len() > 60 {
        return Err(AppError::BadRequest("give the source a name (at most 60 characters)".into()));
    }
    if !KINDS.contains(&src.kind.as_str()) {
        return Err(AppError::BadRequest(format!("unknown kind `{}` ({})", src.kind, KINDS.join(" · "))));
    }
    match src.access.as_str() {
        "server" if src.asset.is_empty() => return Err(AppError::BadRequest("choose the server whose CLI is used".into())),
        "server" | "vault" => {}
        other => return Err(AppError::BadRequest(format!("unknown access `{other}` (server · vault)"))),
    }
    if matches!(src.kind.as_str(), "aws" | "tencent") {
        if src.regions.is_empty() || src.regions.len() > 12 {
            return Err(AppError::BadRequest("name 1 to 12 regions to read".into()));
        }
        for r in &src.regions {
            word(r, "a region", 40)?;
        }
    }
    match src.kind.as_str() {
        "gcp" => word(&src.scope, "the project id", 80)?,
        "azure" => word(&src.scope, "the subscription id", 80)?,
        "kubernetes" if !src.scope.is_empty() => word(&src.scope, "the kubectl context", 200)?,
        _ => {}
    }
    if !src.endpoint.is_empty() || (src.kind == "kubernetes" && src.access == "vault") {
        let ok = (src.endpoint.starts_with("https://") || src.endpoint.starts_with("http://")) && src.endpoint.len() <= 300 && src.endpoint.chars().all(|c| c.is_ascii_graphic()) && !src.endpoint.contains('@');
        if !ok {
            return Err(AppError::BadRequest(if src.kind == "kubernetes" { "give the cluster's API server, like https://10.0.0.1:6443".into() } else { "the endpoint must be an http(s) URL".to_string() }));
        }
    }
    Ok(())
}

pub async fn all(core: &Core) -> AppResult<Vec<Source>> {
    let mut out = Vec::new();
    for key in core.list(&format!("{PREFIX}cfg/")).await? {
        if let Some(s) = core.get_json::<Source>(&key).await? {
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

pub async fn get(core: &Core, id: &str) -> AppResult<Source> {
    core.get_json(&cfg_path(id)).await?.ok_or_else(|| AppError::NotFound(format!("source `{id}`")))
}

pub async fn state(core: &Core, id: &str) -> AppResult<State> {
    Ok(core.get_json(&state_path(id)).await?.unwrap_or_default())
}

pub async fn has_secret(core: &Core, id: &str) -> AppResult<bool> {
    Ok(core.get(&secret_path(id)).await?.is_some())
}

/// Create or change a source. `secret`: the fields given replace the stored
/// ones; none given keeps what is there.
pub async fn save(core: &Core, mut src: Source, secret: BTreeMap<String, String>) -> AppResult<Source> {
    src.name = src.name.trim().to_string();
    src.endpoint = src.endpoint.trim().trim_end_matches('/').to_string();
    src.regions = src.regions.iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
    validate(&src)?;
    let new = src.id.is_empty();
    if new {
        src.id = crate::bastion::random_id(6);
    } else {
        let old = get(core, &src.id).await?;
        if old.kind != src.kind {
            return Err(AppError::BadRequest("a source's kind can't change — add a new one".into()));
        }
    }
    let mut puts = vec![(cfg_path(&src.id), serde_json::to_vec(&src)?)];
    let mut deletes = Vec::new();
    if src.access == "vault" {
        let mut stored: BTreeMap<String, String> = core.get_json(&secret_path(&src.id)).await?.unwrap_or_default();
        for (k, v) in secret {
            if !secret_fields(&src.kind).contains(&k.as_str()) {
                return Err(AppError::BadRequest(format!("`{k}` is not a credential of a {} source", src.kind)));
            }
            if v.len() > 16 * 1024 {
                return Err(AppError::BadRequest(format!("`{k}` is too long")));
            }
            if v.trim().is_empty() {
                stored.remove(&k);
            } else {
                stored.insert(k, v.trim().to_string());
            }
        }
        if let Some(missing) = secret_fields(&src.kind).iter().find(|f| !optional(f) && !stored.contains_key(**f)) {
            return Err(AppError::BadRequest(format!("`{missing}` is needed to read a {} source from the vault", src.kind)));
        }
        puts.push((secret_path(&src.id), serde_json::to_vec(&stored)?));
    } else {
        // Credentials are not kept for a source that does not use them.
        deletes.push(secret_path(&src.id));
    }
    // A changed source is read again at once.
    deletes.push(state_path(&src.id));
    core.commit(puts, deletes, vec![]).await?;
    Ok(src)
}

pub async fn delete(core: &Core, id: &str) -> AppResult<()> {
    get(core, id).await?;
    core.commit(vec![], vec![cfg_path(id), secret_path(id), state_path(id)], vec![]).await
}

/// A server has a `kubectl` that reaches a cluster: read it through that
/// server, unless a cluster source there already exists.
pub async fn offer_cluster(core: &Core, asset: &str, server: &str) -> AppResult<()> {
    if all(core).await?.iter().any(|s| s.kind == "kubernetes" && s.asset == asset) {
        return Ok(());
    }
    let name: String = format!("{server} cluster").chars().take(60).collect();
    save(core, Source { name, kind: "kubernetes".into(), access: "server".into(), asset: asset.into(), ..Default::default() }, BTreeMap::new()).await.map(|_| ())
}

// ── reading ─────────────────────────────────────────────────────────────────

/// Read a source now and store the result (also a failure, with its reason;
/// the last good inventory stays on the map meanwhile).
pub async fn refresh(core: &Core, src: &Source) -> AppResult<State> {
    let old = state(core, &src.id).await?;
    let read = tokio::time::timeout(std::time::Duration::from_secs(120), read(core, src)).await.unwrap_or_else(|_| Err(AppError::Unavailable("reading took longer than 2 minutes".into())));
    let new = match read {
        Ok(mut inventory) => {
            inventory.items.truncate(3000);
            State { at: Some(Utc::now()), error: String::new(), inventory }
        }
        Err(e) => State { at: Some(Utc::now()), error: e.to_string().chars().take(400).collect(), inventory: old.inventory },
    };
    core.commit(vec![(state_path(&src.id), serde_json::to_vec(&new)?)], vec![], vec![]).await?;
    Ok(new)
}

async fn read(core: &Core, src: &Source) -> AppResult<Inventory> {
    let secret: BTreeMap<String, String> = if src.access == "vault" { core.get_json(&secret_path(&src.id)).await?.unwrap_or_default() } else { BTreeMap::new() };
    let via = fetch::Via::open(core, src, secret).await?;
    let res = match src.kind.as_str() {
        "kubernetes" => kube::collect(&via, src).await,
        "aws" => aws::collect(&via, src).await,
        "tencent" => tencent::collect(&via, src).await,
        "gcp" => gcp::collect(&via, src).await,
        "azure" => azure::collect(&via, src).await,
        other => Err(AppError::BadRequest(format!("unknown kind `{other}`"))),
    };
    via.close().await;
    res
}

fn busy() -> &'static Mutex<HashSet<String>> {
    static BUSY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    BUSY.get_or_init(Default::default)
}

/// Called by the collecting instance on every monitoring tick: reads the
/// sources that are due, in the background, one reading of each at a time.
pub async fn tick(core: &std::sync::Arc<Core>) {
    let Ok(sources) = all(core).await else { return };
    // VMs only: no cloud API is called.
    let api = method(core).await.is_ok_and(|m| m == "api");
    let now = Utc::now();
    for src in sources.into_iter().filter(|s| api || !is_cloud(&s.kind)) {
        let due = state(core, &src.id).await.ok().and_then(|s| s.at).is_none_or(|at| (now - at).num_seconds() >= EVERY);
        if !due || !busy().lock().expect("lock").insert(src.id.clone()) {
            continue;
        }
        let core = core.clone();
        tokio::spawn(async move {
            if let Err(e) = refresh(&core, &src).await {
                tracing::warn!(source = %src.name, "sources: could not store the reading: {e}");
            }
            busy().lock().expect("lock").remove(&src.id);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_source_needs() {
        let ok = |s: &Source| validate(s).is_ok();
        let base = Source { name: "prod".into(), kind: "aws".into(), access: "vault".into(), regions: vec!["ap-southeast-1".into()], ..Default::default() };
        assert!(ok(&base));
        assert!(!ok(&Source { name: " ".into(), ..base.clone() }));
        assert!(!ok(&Source { kind: "digitalocean".into(), ..base.clone() }));
        assert!(!ok(&Source { access: "magic".into(), ..base.clone() }));
        assert!(!ok(&Source { regions: vec![], ..base.clone() }), "aws needs regions");
        assert!(!ok(&Source { regions: vec!["us-east-1; id".into()], ..base.clone() }));
        assert!(!ok(&Source { access: "server".into(), ..base.clone() }), "a server must be chosen");
        assert!(ok(&Source { access: "server".into(), asset: "edge-1".into(), ..base.clone() }));
        assert!(!ok(&Source { kind: "gcp".into(), regions: vec![], ..base.clone() }), "gcp needs a project");
        assert!(ok(&Source { kind: "gcp".into(), regions: vec![], scope: "my-project-1".into(), ..base.clone() }));
        assert!(!ok(&Source { kind: "kubernetes".into(), regions: vec![], ..base.clone() }), "the API server is needed");
        assert!(ok(&Source { kind: "kubernetes".into(), regions: vec![], endpoint: "https://10.0.0.1:6443".into(), ..base.clone() }));
        assert!(!ok(&Source { endpoint: "https://user:pw@x".into(), ..base.clone() }));
        assert!(ok(&Source { kind: "kubernetes".into(), access: "server".into(), asset: "edge-1".into(), regions: vec![], scope: "arn:aws:eks:us-east-1:1:cluster/prod".into(), ..base }));
        assert_eq!((tail("https://x/zones/us-central1-a"), tail("plain")), ("us-central1-a", "plain"));
        let v: Value = serde_json::json!({ "a": "x", "n": 80, "one": { "k": 1 }, "many": [1, 2], "empty": "" });
        assert_eq!((s(&v, "a"), s(&v, "n"), num(&v, "n"), list(&v, "one").len(), list(&v, "many").len(), list(&v, "empty").len(), list(&v, "none").len()), ("x".into(), "80".into(), 80, 1, 2, 0, 0));
    }
}
