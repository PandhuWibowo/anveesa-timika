//! The map (docs/MAP.md): what talks to what. Each monitoring reading brings a
//! server's addresses, listeners and open TCP connections — its own and, as
//! root, those inside each container. They are kept as *flows* (who, to
//! where, which port, first / last seen) and, when the map is asked for,
//! resolved against every other server's addresses, published ports and
//! listeners, together with what nginx is configured to forward.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::bastion::Asset;
use crate::grpc::pb;
use crate::monitor::collect::Container;
use crate::sources::{Inventory, Source};

/// Flows not seen for this long are forgotten.
const KEEP_SECS: i64 = 30 * 86400;
const MAX_FLOWS: usize = 1500;
/// A map without changes is still written this often (for `last seen`).
const SAVE_EVERY: i64 = 300;

pub fn path(asset: &str) -> String {
    format!("monitor/topo/{asset}")
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Listener {
    /// tcp · udp
    pub proto: String,
    pub addr: String,
    pub port: u16,
    pub process: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CNet {
    pub name: String,
    /// (network, address)
    pub ips: Vec<(String, String)>,
    pub project: String,
    pub listens: Vec<u16>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conn {
    /// A container's name, or empty for the server itself.
    pub owner: String,
    pub local: (String, u16),
    pub peer: (String, u16),
    pub process: String,
}

/// What one reading says about the network.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetRaw {
    pub ips: Vec<String>,
    pub listeners: Vec<Listener>,
    pub containers: Vec<CNet>,
    pub conns: Vec<Conn>,
    /// Connections inside containers were readable (root + nsenter).
    pub deep: bool,
}

/// "10.0.0.5:443", "[::1]:80", "*:22", "127.0.0.53%lo:53" → (address, port).
fn endpoint(s: &str) -> Option<(String, u16)> {
    let (a, p) = s.trim().rsplit_once(':')?;
    let a = a.trim_matches(['[', ']']).split('%').next().unwrap_or_default();
    let a = a.strip_prefix("::ffff:").unwrap_or(a);
    let a = if a == "*" || a.is_empty() { "0.0.0.0" } else { a };
    let ok = a.len() <= 45 && a.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, '.' | ':'));
    Some((a.to_string(), p.parse().ok()?)).filter(|_| ok)
}

fn word(s: &str, max: usize) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ':' | '@' | '+')).take(max).collect()
}

impl NetRaw {
    /// One line of the collector's output.
    pub fn line(&mut self, k: &str, v: &str) {
        let f: Vec<&str> = v.split('|').collect();
        match k {
            "ip" => {
                if let Some((a, _)) = endpoint(&format!("{v}:0")).filter(|_| self.ips.len() < 60) {
                    self.ips.push(a);
                }
            }
            "lsn" if f.len() >= 2 && self.listeners.len() < 300 => {
                if let Some((addr, port)) = endpoint(f[1]) {
                    let l = Listener { proto: if f[0] == "udp" { "udp".into() } else { "tcp".into() }, addr, port, process: word(f.get(2).copied().unwrap_or_default(), 40) };
                    if !self.listeners.contains(&l) {
                        self.listeners.push(l);
                    }
                }
            }
            "con" | "ccn" if f.len() >= 3 && self.conns.len() < 6000 => {
                if let (Some(local), Some(peer)) = (endpoint(f[1]), endpoint(f[2])) {
                    self.conns.push(Conn { owner: word(f[0], 128), local, peer, process: word(f.get(3).copied().unwrap_or_default(), 40) });
                }
            }
            "cin" if f.len() >= 3 && self.containers.len() < 60 => {
                let ips = f[2].split(',').filter_map(|x| x.split_once('=')).filter(|x| !x.1.is_empty()).filter_map(|(n, a)| Some((word(n, 80), endpoint(&format!("{a}:0"))?.0))).collect();
                self.containers.push(CNet { name: word(f[0].trim_start_matches('/'), 128), ips, project: word(f.get(3).copied().unwrap_or_default(), 80), listens: vec![] });
            }
            "cls" if f.len() >= 2 => {
                if let (Some(c), Some((_, port))) = (self.containers.iter_mut().find(|c| c.name == f[0]), endpoint(f[1])) {
                    if !c.listens.contains(&port) && c.listens.len() < 50 {
                        c.listens.push(port);
                    }
                }
            }
            "deep" => self.deep = v == "1",
            _ => {}
        }
    }
}

/// One direction of traffic seen on a server: `owner` (a container, or the
/// server's `process`) connects out to `peer:port`, or is connected to on `port`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Flow {
    pub out: bool,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub process: String,
    /// An address, or `public` for clients on the internet (never kept one by one).
    pub peer: String,
    pub port: u16,
    pub first: i64,
    pub last: i64,
    /// Readings it was seen in.
    pub seen: u32,
    /// Most connections (out) or distinct clients (in) in one reading.
    #[serde(default)]
    pub peak: u32,
}

/// One nginx site and where its configuration forwards to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Route {
    /// The nginx container, or empty for nginx on the server.
    pub slot: String,
    pub site: String,
    pub ports: Vec<u16>,
    pub tls: bool,
    /// host:port
    pub backends: Vec<String>,
}

/// Asked of the server itself, every few minutes: which cloud it runs in (its
/// metadata service answers without credentials; only tried when the machine's
/// DMI names a cloud), its default gateway and resolvers, and whether a working
/// `kubectl` is there.
pub const ENV_SCRIPT: &str = r##"# timika-cloud v1
PATH="$PATH:/usr/sbin:/sbin:/usr/local/bin"
V="$(cat /sys/class/dmi/id/sys_vendor /sys/class/dmi/id/product_name /sys/class/dmi/id/bios_vendor /sys/class/dmi/id/chassis_asset_tag 2>/dev/null | tr '\n' ' ')"
if command -v curl >/dev/null 2>&1; then g() { curl -s -f -m 2 --noproxy '*' "$@" 2>/dev/null; }; else g() { return 1; }; fi
case "$V" in
*Tencent*)
  B=http://metadata.tencentyun.com/latest/meta-data
  k() { echo "$1=$(g "$B/$2" | head -c 300 | tr '\r\n' '  ')"; }
  echo cloud=tencent; k id instance-id; k name instance-name; k type instance/instance-type; k region placement/region; k zone placement/zone; k private local-ipv4; k public public-ipv4
  MAC=$(g "$B/mac"); IP=$(g "$B/local-ipv4"); N="network/interfaces/macs/$MAC"
  k vpc "$N/vpc-id"; k subnet "$N/subnet-id"; k vpc_cidr "$N/vpc-cidr-block"; k subnet_cidr "$N/subnet-cidr-block"
  k mask "$N/local-ipv4s/$IP/subnet-mask"; k public_mode "$N/local-ipv4s/$IP/public-ipv4-mode"
  k image instance/image-id; k bw_out instance/bandwidth-limit-egress; k bw_in instance/bandwidth-limit-ingress ;;
*Amazon*|*amazon*)
  B=http://169.254.169.254/latest
  T=$(curl -s -f -m 2 -X PUT -H 'X-aws-ec2-metadata-token-ttl-seconds: 60' "$B/api/token" 2>/dev/null)
  k() { echo "$1=$(g -H "X-aws-ec2-metadata-token: $T" "$B/meta-data/$2" | head -c 300 | tr '\r\n' '  ')"; }
  echo cloud=aws; k id instance-id; k type instance-type; k region placement/region; k zone placement/availability-zone; k private local-ipv4; k public public-ipv4
  MAC=$(g -H "X-aws-ec2-metadata-token: $T" "$B/meta-data/mac"); N="network/interfaces/macs/$MAC"
  k vpc "$N/vpc-id"; k subnet "$N/subnet-id"; k vpc_cidr "$N/vpc-ipv4-cidr-block"; k subnet_cidr "$N/subnet-ipv4-cidr-block"
  k groups security-groups; k image ami-id ;;
*Google*)
  B=http://169.254.169.254/computeMetadata/v1
  k() { echo "$1=$(g -H 'Metadata-Flavor: Google' "$B/$2" | head -c 300 | tr '\r\n' '  ')"; }
  echo cloud=gcp; k id instance/id; k name instance/name; k type instance/machine-type; k zone instance/zone; k private instance/network-interfaces/0/ip; k public instance/network-interfaces/0/access-configs/0/external-ip; k vpc instance/network-interfaces/0/network; k project project/project-id
  k mask instance/network-interfaces/0/subnetmask; k image instance/image ;;
*Microsoft*|*7783-7084-3265-9085-8269-3286-77*)
  echo cloud=azure
  echo "azure=$(g -H Metadata:true 'http://169.254.169.254/metadata/instance?api-version=2021-02-01' | head -c 8000 | tr '\r\n' '  ')" ;;
esac
echo "gw=$(ip route show default 2>/dev/null | awk '{ print $3; exit }')"
# Every interface with its prefix, and the routing table.
ip -o addr show 2>/dev/null | awk '$3 == "inet" || $3 == "inet6" { if ($4 !~ /^(127\.|::1\/|fe80:)/) print "ifc=" $2 "|" $4 }' | head -n 40
ip route show 2>/dev/null | awk '{ via = ""; dev = ""; for (i = 1; i < NF; i++) { if ($i == "via") via = $(i + 1); if ($i == "dev") dev = $(i + 1) } print "route=" $1 "|" via "|" dev }' | head -n 40
echo "dns=$(awk '/^nameserver/ { printf "%s ", $2 }' /etc/resolv.conf 2>/dev/null)"
K=0; if command -v kubectl >/dev/null 2>&1 && kubectl get --raw /version --request-timeout=3s >/dev/null 2>&1; then K=1; fi
echo "k8s=$K"
echo end=1
"##;

/// The cloud a server runs in, as its own metadata service tells it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CloudInfo {
    /// tencent · aws · gcp · azure
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub zone: String,
    #[serde(default)]
    pub vpc: String,
    #[serde(default)]
    pub subnet: String,
    #[serde(default)]
    pub private: String,
    #[serde(default)]
    pub public: String,
    /// The address ranges of its network and subnet.
    #[serde(default)]
    pub vpc_cidr: String,
    #[serde(default)]
    pub subnet_cidr: String,
    /// How the public address is attached (Tencent: EIP or not).
    #[serde(default)]
    pub public_mode: String,
    #[serde(default)]
    pub image: String,
    /// Mbit/s, when the cloud caps it.
    #[serde(default)]
    pub bandwidth: String,
    /// AWS: the security groups.
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Iface {
    pub name: String,
    /// "10.3.19.192/20"
    pub cidr: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteEntry {
    /// "default", "172.17.0.0/16"
    pub dest: String,
    pub via: String,
    pub dev: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Env {
    #[serde(default)]
    pub cloud: Option<CloudInfo>,
    #[serde(default)]
    pub gateway: String,
    #[serde(default)]
    pub dns: Vec<String>,
    /// A `kubectl` that reaches a cluster.
    #[serde(default)]
    pub k8s: bool,
    #[serde(default)]
    pub interfaces: Vec<Iface>,
    #[serde(default)]
    pub routes: Vec<RouteEntry>,
}

/// "10.3.19.192" + "255.255.240.0" (or a prefix length) → "10.3.16.0/20".
pub fn network(ip: &str, mask: &str) -> String {
    let Some(std::net::IpAddr::V4(a)) = parse_ip(ip) else { return String::new() };
    let bits = match mask.parse::<u32>() {
        Ok(n) if n <= 32 => n,
        _ => match parse_ip(mask) {
            Some(std::net::IpAddr::V4(m)) => u32::from(m).count_ones(),
            _ => return String::new(),
        },
    };
    let m = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
    format!("{}/{bits}", std::net::Ipv4Addr::from(u32::from(a) & m))
}

/// A CIDR block, or nothing.
fn cidr(v: &str) -> String {
    let v = v.trim();
    match v.split_once('/') {
        Some((a, p)) if parse_ip(a).is_some() && p.parse::<u8>().is_ok_and(|p| p <= 128) => v.to_string(),
        _ => String::new(),
    }
}

/// What ENV_SCRIPT printed.
pub fn parse_env(out: &str) -> Env {
    let mut e = Env::default();
    let mut c = CloudInfo::default();
    let mut mask = String::new();
    let last = |v: &str| v.trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string();
    let addr = |v: &str| if v.trim().is_empty() { String::new() } else { endpoint(&format!("{}:0", v.trim())).map(|x| x.0).unwrap_or_default() };
    for l in out.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = v.trim();
        match k {
            "cloud" => c.provider = word(v, 12),
            "id" => c.id = word(v, 80),
            "name" => c.name = word(v, 80),
            // Google answers with paths: "projects/1/zones/us-central1-a".
            "type" => c.kind = word(&last(v), 60),
            "region" => c.region = word(v, 40),
            "zone" => c.zone = word(&last(v), 40),
            "vpc" => c.vpc = word(&last(v), 80),
            "subnet" => c.subnet = word(&last(v), 80),
            "private" => c.private = addr(v),
            "public" => c.public = addr(v),
            "azure" => {
                if let Ok(j) = serde_json::from_str::<serde_json::Value>(v) {
                    let (comp, net) = (&j["compute"], &j["network"]["interface"][0]["ipv4"]);
                    let s = |x: &serde_json::Value| word(x.as_str().unwrap_or_default(), 80);
                    c.id = s(&comp["vmId"]);
                    c.name = s(&comp["name"]);
                    c.kind = s(&comp["vmSize"]);
                    c.region = s(&comp["location"]);
                    c.zone = [s(&comp["location"]), s(&comp["zone"])].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
                    c.vpc = s(&comp["resourceGroupName"]);
                    c.subnet = format!("{}/{}", s(&net["subnet"][0]["address"]), s(&net["subnet"][0]["prefix"])).trim_matches('/').to_string();
                    c.private = addr(net["ipAddress"][0]["privateIpAddress"].as_str().unwrap_or_default());
                    c.public = addr(net["ipAddress"][0]["publicIpAddress"].as_str().unwrap_or_default());
                }
            }
            "vpc_cidr" => c.vpc_cidr = cidr(v),
            "subnet_cidr" => c.subnet_cidr = cidr(v),
            "mask" => mask = v.to_string(),
            "public_mode" => c.public_mode = word(v, 20),
            "image" => c.image = word(&last(v), 80),
            "bw_out" | "bw_in" => {
                if v.parse::<u32>().is_ok_and(|n| n > 0) {
                    let side = if k == "bw_out" { "out" } else { "in" };
                    c.bandwidth = [c.bandwidth.clone(), format!("{v} Mbit/s {side}")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
                }
            }
            "groups" => c.groups = v.split_whitespace().map(|x| word(x, 80)).filter(|x| !x.is_empty()).take(10).collect(),
            "ifc" => {
                if let Some((n, a)) = v.split_once('|') {
                    let a = cidr(a);
                    let local = a.starts_with("fe80:") || a.starts_with("127.") || a.starts_with("::1/");
                    if !a.is_empty() && !local && e.interfaces.len() < 40 {
                        e.interfaces.push(Iface { name: word(n.trim_end_matches(':'), 30), cidr: a });
                    }
                }
            }
            "route" => {
                let f: Vec<&str> = v.split('|').collect();
                if f.len() == 3 && e.routes.len() < 40 && (f[0] == "default" || !cidr(f[0]).is_empty() || parse_ip(f[0]).is_some()) {
                    e.routes.push(RouteEntry { dest: f[0].into(), via: addr(f[1]), dev: word(f[2], 30) });
                }
            }
            "gw" => e.gateway = addr(v),
            "dns" => e.dns = v.split_whitespace().map(addr).filter(|x| !x.is_empty()).take(4).collect(),
            "k8s" => e.k8s = v == "1",
            _ => {}
        }
    }
    // The subnet's range: from the metadata, else the private address and its mask or interface prefix.
    if c.subnet_cidr.is_empty() && !c.private.is_empty() {
        let prefix = e.interfaces.iter().find(|i| i.cidr.starts_with(&format!("{}/", c.private))).and_then(|i| i.cidr.split('/').nth(1).map(String::from));
        c.subnet_cidr = network(&c.private, if mask.is_empty() { prefix.as_deref().unwrap_or_default() } else { &mask });
    }
    if c.region.is_empty() {
        // "us-central1-a" → "us-central1"
        c.region = c.zone.rsplit_once('-').map(|x| x.0.to_string()).unwrap_or_default();
    }
    // Only when the metadata service really answered.
    if !c.provider.is_empty() && !c.id.is_empty() {
        e.cloud = Some(c);
    }
    e
}

/// Addresses a cloud's load balancers check their targets from: a peer in
/// these ranges means "there is a load balancer in front of this server".
fn health_check(provider: &str, peer: &str) -> bool {
    let Some(std::net::IpAddr::V4(v)) = parse_ip(peer) else { return false };
    let o = v.octets();
    match provider {
        // CLB checks and forwards from 100.64.0.0/10.
        "tencent" => o[0] == 100 && o[1] & 0xc0 == 64,
        "gcp" => (o[0] == 35 && o[1] == 191) || (o[0] == 130 && o[1] == 211 && o[2] & 0xfc == 0),
        "azure" => o == [168, 63, 129, 16],
        _ => false,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Topo {
    pub time: i64,
    #[serde(default)]
    pub ips: Vec<String>,
    #[serde(default)]
    pub listeners: Vec<Listener>,
    #[serde(default)]
    pub containers: Vec<CNet>,
    #[serde(default)]
    pub flows: Vec<Flow>,
    #[serde(default)]
    pub routes: Vec<Route>,
    #[serde(default)]
    pub routes_at: i64,
    #[serde(default)]
    pub deep: bool,
    /// Where the server is: its cloud, gateway, resolvers.
    #[serde(default)]
    pub env: Option<Env>,
    /// A cluster reachable from this server was offered as a source once.
    #[serde(default)]
    pub k8s_offered: bool,
    #[serde(skip)]
    pub saved: i64,
}

fn parse_ip(a: &str) -> Option<std::net::IpAddr> {
    a.parse().ok()
}
fn loopback(a: &str) -> bool {
    parse_ip(a).is_some_and(|i| i.is_loopback() || i.is_unspecified())
}
/// Not an address of a network you run: a client or a service on the internet.
pub fn public(a: &str) -> bool {
    match parse_ip(a) {
        Some(std::net::IpAddr::V4(v)) => !(v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || (v.octets()[0] == 100 && v.octets()[1] & 0xc0 == 64)),
        Some(std::net::IpAddr::V6(v)) => !(v.is_loopback() || v.is_unspecified() || v.segments()[0] & 0xfe00 == 0xfc00 || v.segments()[0] & 0xffc0 == 0xfe80),
        None => false,
    }
}
/// The same /16 — a container network's gateway, seen from inside a container.
fn near(a: &str, b: &str) -> bool {
    match (parse_ip(a), parse_ip(b)) {
        (Some(std::net::IpAddr::V4(x)), Some(std::net::IpAddr::V4(y))) => x.octets()[..2] == y.octets()[..2],
        _ => false,
    }
}

impl Topo {
    pub fn routes_due(&self, now: i64) -> bool {
        now - self.routes_at >= 600
    }

    /// Take in one reading. True when the stored map should be written.
    pub fn absorb(&mut self, raw: &NetRaw, now: i64) -> bool {
        let host_ports: HashSet<u16> = raw.listeners.iter().filter(|l| l.proto == "tcp").map(|l| l.port).collect();
        let all_cips: HashSet<&str> = raw.containers.iter().flat_map(|c| c.ips.iter().map(|i| i.1.as_str())).collect();
        // key → (connections, distinct peers)
        let mut found: BTreeMap<(bool, String, String, String, u16), (u32, HashSet<&str>)> = BTreeMap::new();
        for c in &raw.conns {
            let cnet = raw.containers.iter().find(|x| x.name == c.owner);
            if !c.owner.is_empty() && cnet.is_none() {
                continue;
            }
            let own: Vec<&str> = match cnet {
                Some(n) => n.ips.iter().map(|i| i.1.as_str()).collect(),
                None => raw.ips.iter().map(String::as_str).collect(),
            };
            let inbound = match cnet {
                Some(n) => n.listens.contains(&c.local.1),
                None => host_ports.contains(&c.local.1),
            };
            let peer = c.peer.0.as_str();
            let key = if inbound {
                // The other side of a local connection is recorded as its `out`.
                if loopback(peer) || own.contains(&peer) || (cnet.is_none() && all_cips.contains(peer)) {
                    continue;
                }
                // Through the runtime's proxy / gateway: the published port is recorded on the server.
                if cnet.is_some() && !all_cips.contains(peer) && own.iter().any(|o| near(o, peer)) {
                    continue;
                }
                let provider = self.env.as_ref().and_then(|e| e.cloud.as_ref()).map(|c| c.provider.as_str()).unwrap_or_default();
                // A cloud load balancer's health checks: one flow, whatever address they come from.
                let who = if cnet.is_none() && health_check(provider, peer) { "lb".to_string() } else if public(peer) { "public".to_string() } else { peer.to_string() };
                (false, c.owner.clone(), c.process.clone(), who, c.local.1)
            } else {
                // The runtime's port proxy is plumbing, not a client.
                if c.process == "docker-proxy" || (cnet.is_some() && loopback(peer)) {
                    continue;
                }
                (true, c.owner.clone(), c.process.clone(), peer.to_string(), c.peer.1)
            };
            let e = found.entry(key).or_default();
            e.0 += 1;
            e.1.insert(peer);
        }
        let mut new = false;
        for ((out, owner, process, peer, port), (n, peers)) in found {
            let peak = if out { n } else { peers.len() as u32 };
            match self.flows.iter_mut().find(|f| f.out == out && f.owner == owner && f.process == process && f.peer == peer && f.port == port) {
                Some(f) => {
                    f.last = now;
                    f.seen += 1;
                    f.peak = f.peak.max(peak);
                }
                None => {
                    self.flows.push(Flow { out, owner, process, peer, port, first: now, last: now, seen: 1, peak });
                    new = true;
                }
            }
        }
        self.flows.retain(|f| now - f.last < KEEP_SECS);
        if self.flows.len() > MAX_FLOWS {
            self.flows.sort_by_key(|f| std::cmp::Reverse(f.last));
            self.flows.truncate(MAX_FLOWS);
        }
        let changed = new || self.ips != raw.ips || self.listeners != raw.listeners || self.containers != raw.containers || self.deep != raw.deep;
        self.time = now;
        self.ips = raw.ips.clone();
        self.listeners = raw.listeners.clone();
        self.containers = raw.containers.clone();
        self.deep = raw.deep;
        if changed || now - self.saved >= SAVE_EVERY {
            self.saved = now;
            return true;
        }
        false
    }
}

/// What nginx's sites forward to, from its parsed configuration.
pub fn routes(slot: &str, inst: &pb::NginxInstance) -> Vec<Route> {
    inst.sites
        .iter()
        .filter(|s| s.enabled && !s.backends.is_empty())
        .take(100)
        .map(|s| Route {
            slot: slot.to_string(),
            site: s.names.first().filter(|n| *n != "_").cloned().unwrap_or_else(|| format!(":{}", s.listens.first().map(|l| l.split(' ').next().unwrap_or_default()).unwrap_or_default())),
            ports: s.listens.iter().filter_map(|l| l.split(' ').next()?.rsplit(':').next()?.parse().ok()).collect(),
            tls: s.tls,
            backends: s.backends.clone(),
        })
        .collect()
}

// ── building the map ────────────────────────────────────────────────────────

pub struct Input {
    pub asset: Asset,
    pub topo: Topo,
    pub containers: Vec<Container>,
    /// pending · up · down
    pub status: String,
}

/// "0.0.0.0:8080->80/tcp, [::]:8080->80/tcp" → [(8080, 80)]
fn published(ports: &str) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for p in ports.split(',') {
        let Some((host, inner)) = p.trim().split_once("->") else { continue };
        let (Some(h), Some(c)) = (host.rsplit(':').next().and_then(|x| x.parse().ok()), inner.split('/').next().and_then(|x| x.parse().ok())) else { continue };
        if !out.contains(&(h, c)) {
            out.push((h, c));
        }
    }
    out
}

struct Site<'a> {
    i: &'a Input,
    /// host port → (container, its port)
    published: HashMap<u16, (String, u16)>,
    /// container address → container
    cip: HashMap<&'a str, &'a str>,
    /// tcp port → the process listening on it
    lproc: HashMap<u16, &'a str>,
}

/// Where an item of a source is on the map: its own node, or one of your servers.
#[derive(Clone)]
enum At {
    Node(String),
    Server(usize),
}

struct Builder<'a> {
    sites: Vec<Site<'a>>,
    by_ip: HashMap<String, usize>,
    /// An address of a load balancer, NAT gateway, service, pod or unmonitored VM → its node.
    ip_node: HashMap<String, String>,
    /// Instance ids, ARNs, DNS names (lower case) → where that is.
    refs: HashMap<String, At>,
    /// A Kubernetes node port → the service behind it.
    node_ports: HashMap<u16, String>,
    nodes: BTreeMap<String, pb::MapNode>,
    edges: BTreeMap<(String, String, u32), pb::MapEdge>,
}

fn iso(t: i64) -> String {
    DateTime::<Utc>::from_timestamp(t, 0).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_default()
}

impl<'a> Builder<'a> {
    fn server(&self, i: usize) -> String {
        format!("s:{}", self.sites[i].i.asset.id)
    }
    fn container(&self, i: usize, name: &str) -> String {
        format!("c:{}:{name}", self.sites[i].i.asset.id)
    }
    /// A process on a server; made on first use (a client has no listener).
    fn process(&mut self, i: usize, name: &str) -> String {
        if name.is_empty() {
            return self.server(i);
        }
        let a = &self.sites[i].i.asset;
        let id = format!("p:{}:{name}", a.id);
        self.nodes.entry(id.clone()).or_insert_with(|| pb::MapNode { id: id.clone(), kind: "process".into(), label: name.to_string(), asset: a.id.clone(), server: a.name.clone(), ..Default::default() });
        id
    }
    /// Whatever answers on `port` of server `i`: a published container, the listening process, or the server.
    fn service(&mut self, i: usize, port: u16) -> (String, String) {
        if let Some((c, inner)) = self.sites[i].published.get(&port).cloned() {
            let note = if inner != port { format!("published port {port} → {inner}") } else { String::new() };
            return (self.container(i, &c), note);
        }
        match self.sites[i].lproc.get(&port).copied() {
            Some(p) => (self.process(i, p), String::new()),
            None => (self.server(i), String::new()),
        }
    }
    fn outside(&mut self, addr: &str) -> String {
        let id = format!("x:{addr}");
        // 169.254.0.0/16 is never another machine of yours: it is the platform under the server.
        let platform = matches!(parse_ip(addr), Some(std::net::IpAddr::V4(v)) if v.is_link_local());
        let attrs = if platform { vec!["What it is: a link-local address — the cloud platform's own services (metadata, monitoring and security agents)".to_string()] } else { Vec::new() };
        self.nodes.entry(id.clone()).or_insert_with(|| pb::MapNode { id: id.clone(), kind: "external".into(), label: addr.to_string(), public: public(addr) || parse_ip(addr).is_none(), attrs, ..Default::default() });
        id
    }
    fn internet(&mut self) -> String {
        self.nodes.entry("internet".into()).or_insert_with(|| pb::MapNode { id: "internet".into(), kind: "internet".into(), label: "Internet".into(), public: true, ..Default::default() });
        "internet".into()
    }
    /// Where `addr:port` leads, seen from server `i` (and from container `owner` on it).
    fn resolve(&mut self, i: usize, owner: &str, addr: &str, port: u16) -> Option<(String, String)> {
        let own = self.sites[i].i.topo.ips.iter().any(|x| x == addr);
        if loopback(addr) || own {
            return if !owner.is_empty() && loopback(addr) { None } else { Some(self.service(i, port)) };
        }
        if let Some(c) = self.sites[i].cip.get(addr).copied() {
            return Some((self.container(i, c), String::new()));
        }
        // A container reaching its server through the network's gateway.
        if !owner.is_empty() && self.sites[i].i.topo.containers.iter().filter(|c| c.name == owner).flat_map(|c| &c.ips).any(|x| near(&x.1, addr)) {
            return Some(self.service(i, port));
        }
        if let Some(j) = self.by_ip.get(addr).copied() {
            return Some(self.service(j, port));
        }
        if let Some(id) = self.ip_node.get(addr) {
            return Some((id.clone(), String::new()));
        }
        Some((self.outside(addr), String::new()))
    }

    fn at(&mut self, at: At, port: u16, receiving: bool) -> (String, String) {
        match at {
            At::Node(id) => (id, String::new()),
            // Traffic to a server lands on whatever answers on that port there.
            At::Server(j) if receiving && port > 0 => self.service(j, port),
            At::Server(j) => (self.server(j), String::new()),
        }
    }

    /// One end of a source's link (see `sources::Link`).
    fn end(&mut self, items: &HashMap<String, At>, spec: &str, port: u16, receiving: bool) -> Option<(String, String)> {
        if spec == "internet" {
            return Some((self.internet(), String::new()));
        }
        let (kind, v) = spec.split_once(':')?;
        match kind {
            "id" => items.get(v).cloned().map(|a| self.at(a, port, receiving)),
            "ref" => {
                let at = self.refs.get(&v.to_lowercase()).cloned()?;
                Some(self.at(at, port, receiving))
            }
            "ip" | "ext" => {
                if let Some(j) = self.by_ip.get(v).copied() {
                    return Some(if receiving { self.service(j, port) } else { (self.server(j), String::new()) });
                }
                if let Some(id) = self.ip_node.get(v) {
                    return Some((id.clone(), String::new()));
                }
                if let Some(at) = self.refs.get(&v.to_lowercase()).cloned() {
                    return Some(self.at(at, port, receiving));
                }
                // A cloud's load balancer nobody told us about, or simply the internet.
                Some(if kind == "ext" && (public(v) || parse_ip(v).is_none()) { (self.internet(), String::new()) } else { (self.outside(v), String::new()) })
            }
            _ => None,
        }
    }
    /// A name from a configuration file: a container beside it, one of your servers, or something else.
    fn resolve_name(&mut self, i: usize, slot: &str, host: &str, port: u16) -> Option<(String, String)> {
        if host == "localhost" {
            return self.resolve(i, slot, "127.0.0.1", port);
        }
        if parse_ip(host).is_some() {
            return self.resolve(i, slot, host, port);
        }
        // Compose names services "project-service-1"; in its network the service answers to "service".
        let hit = self.sites[i].i.containers.iter().map(|c| c.name.as_str()).find(|n| *n == host || n.strip_suffix("-1").or(n.strip_suffix("_1")).is_some_and(|b| b.ends_with(&format!("-{host}")) || b.ends_with(&format!("_{host}"))));
        if let Some(c) = hit {
            return Some((self.container(i, c), String::new()));
        }
        if let Some(j) = self.sites.iter().position(|s| s.i.asset.host.eq_ignore_ascii_case(host) || s.i.asset.name.eq_ignore_ascii_case(host)) {
            return Some(self.service(j, port));
        }
        Some((self.outside(host), String::new()))
    }
    fn edge(&mut self, from: String, to: String, port: u16) -> &mut pb::MapEdge {
        self.edges.entry((from.clone(), to.clone(), port as u32)).or_insert_with(|| pb::MapEdge { from, to, port: port as u32, ..Default::default() })
    }
    fn observe(&mut self, from: String, to: String, f: &Flow, note: String, from_process: &str, to_process: &str) {
        let e = self.edge(from, to, f.port);
        e.observed = true;
        e.first_seen = if e.first_seen.is_empty() { iso(f.first) } else { e.first_seen.clone().min(iso(f.first)) };
        e.last_seen = e.last_seen.clone().max(iso(f.last));
        e.seen = e.seen.max(f.seen);
        e.peak = e.peak.max(f.peak);
        if e.note.is_empty() {
            e.note = note;
        }
        if e.from_process.is_empty() {
            e.from_process = from_process.to_string();
        }
        if e.to_process.is_empty() {
            e.to_process = to_process.to_string();
        }
    }
}

/// Everything the caller may see, resolved into nodes and edges. Flows last
/// seen before `since` are left out.
#[cfg(test)]
pub fn build(inputs: &[Input], since: i64) -> pb::NetworkMap {
    build_with(inputs, &[], since)
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "kubernetes" => "Kubernetes",
        "aws" => "AWS",
        "tencent" => "Tencent Cloud",
        "gcp" => "Google Cloud",
        "azure" => "Azure",
        _ => "Cloud",
    }
}

/// The map with what the sources (clusters, cloud accounts) know. A VM or a
/// cluster node that is one of your monitored servers is that server; every
/// address of a load balancer, NAT gateway, service or pod gets its name.
pub fn build_with(inputs: &[Input], sources: &[(Source, Inventory)], since: i64) -> pb::NetworkMap {
    let mut b = Builder { sites: Vec::new(), by_ip: HashMap::new(), ip_node: HashMap::new(), refs: HashMap::new(), node_ports: HashMap::new(), nodes: BTreeMap::new(), edges: BTreeMap::new() };
    let mut notes = Vec::new();
    for (n, i) in inputs.iter().enumerate() {
        let mut published_map = HashMap::new();
        for c in &i.containers {
            for (h, inner) in published(&c.ports) {
                published_map.insert(h, (c.name.clone(), inner));
            }
        }
        b.sites.push(Site {
            i,
            published: published_map,
            cip: i.topo.containers.iter().flat_map(|c| c.ips.iter().map(move |x| (x.1.as_str(), c.name.as_str()))).collect(),
            lproc: i.topo.listeners.iter().filter(|l| l.proto == "tcp" && !l.process.is_empty() && l.process != "docker-proxy").map(|l| (l.port, l.process.as_str())).collect(),
        });
        for ip in i.topo.ips.iter().chain(parse_ip(&i.asset.host).map(|_| &i.asset.host)) {
            b.by_ip.entry(ip.clone()).or_insert(n);
        }
    }
    // Every server, what listens on it, and its containers — connected or not.
    for (n, i) in inputs.iter().enumerate() {
        let a = &i.asset;
        b.nodes.insert(b.server(n), pb::MapNode { id: b.server(n), kind: "server".into(), label: a.name.clone(), asset: a.id.clone(), server: a.name.clone(), detail: a.host.clone(), addresses: i.topo.ips.clone(), state: i.status.clone(), ..Default::default() });
        for l in i.topo.listeners.iter().filter(|l| l.process != "docker-proxy") {
            let id = if l.process.is_empty() { b.server(n) } else { b.process(n, &l.process) };
            let node = b.nodes.get_mut(&id).expect("node");
            let p = pb::MapPort { port: l.port as u32, proto: l.proto.clone(), bind: l.addr.clone(), local: loopback(&l.addr) && l.addr != "0.0.0.0" && l.addr != "::" };
            if !node.ports.contains(&p) {
                node.ports.push(p);
            }
        }
        for c in &i.containers {
            let net = i.topo.containers.iter().find(|x| x.name == c.name);
            let mut ports: Vec<pb::MapPort> = published(&c.ports).into_iter().map(|(h, inner)| pb::MapPort { port: inner as u32, proto: "tcp".into(), bind: format!("published on {h}"), local: false }).collect();
            for p in net.map(|x| x.listens.as_slice()).unwrap_or_default() {
                if !ports.iter().any(|x| x.port == *p as u32) {
                    ports.push(pb::MapPort { port: *p as u32, proto: "tcp".into(), bind: "inside its network".into(), local: true });
                }
            }
            let id = b.container(n, &c.name);
            b.nodes.insert(id.clone(), pb::MapNode { id, kind: "container".into(), label: c.name.clone(), asset: a.id.clone(), server: a.name.clone(), detail: c.image.clone(), addresses: net.map(|x| x.ips.iter().map(|i| format!("{} ({})", i.1, i.0)).collect()).unwrap_or_default(), ports, state: c.state.clone(), project: net.map(|x| x.project.clone()).unwrap_or_default(), ..Default::default() });
        }
        if !i.topo.deep && i.containers.iter().any(|c| c.state == "running") {
            notes.push(format!("{}: connections between containers are not visible — the monitoring account is not root there", a.name));
        }
        if i.topo.time == 0 {
            notes.push(format!("{}: no network reading yet", a.name));
        }
    }
    // Where each server says it is: its cloud, zone, network and public address.
    let mut pub_node: Vec<Option<String>> = vec![None; inputs.len()];
    let mut auto_card: Vec<Option<String>> = vec![None; inputs.len()];
    for (n, i) in inputs.iter().enumerate() {
        let Some(env) = &i.topo.env else { continue };
        let sid = b.server(n);
        let mut facts = Vec::new();
        if let Some(c) = &env.cloud {
            let label = kind_label(&c.provider);
            facts.push(format!("{label}: {}", [c.id.as_str(), c.name.as_str(), c.kind.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ")));
            let with = |id: &str, range: &str| if range.is_empty() { id.to_string() } else { format!("{id} ({range})") };
            let public = if c.public.is_empty() { String::new() } else if c.public_mode.is_empty() { c.public.clone() } else { format!("{} ({})", c.public, c.public_mode) };
            for (k, v) in [("Zone", c.zone.clone()), ("VPC", with(&c.vpc, &c.vpc_cidr)), ("Subnet", with(&c.subnet, &c.subnet_cidr)), ("Private address", c.private.clone()), ("Public address", public), ("Image", c.image.clone()), ("Bandwidth", c.bandwidth.clone()), ("Security groups", c.groups.join(", "))] {
                if !v.is_empty() {
                    facts.push(format!("{k}: {v}"));
                }
            }
            for ip in [&c.private, &c.public] {
                if !ip.is_empty() {
                    b.by_ip.entry(ip.clone()).or_insert(n);
                }
            }
            // One card per cloud network: what stands between its servers and the internet.
            let gid = format!("g:auto:{}:{}", c.provider, if c.vpc.is_empty() { &c.region } else { &c.vpc });
            b.nodes.entry(gid.clone()).or_insert_with(|| pb::MapNode { id: gid.clone(), kind: "cloud".into(), label: format!("{label} · {}", if c.region.is_empty() { &c.zone } else { &c.region }), detail: c.vpc.clone(), group: gid.clone(), attrs: vec!["Found by: the servers' own metadata service (no credentials)".into()], ..Default::default() });
            auto_card[n] = Some(gid.clone());
            // The network and the subnet, with their address ranges.
            if !c.vpc.is_empty() {
                let id = format!("i:auto:{}:vpc:{}", c.provider, c.vpc);
                b.nodes.entry(id.clone()).or_insert_with(|| pb::MapNode { id: id.clone(), kind: "vpc".into(), label: c.vpc.clone(), server: label.into(), detail: c.vpc_cidr.clone(), addresses: if c.vpc_cidr.is_empty() { vec![] } else { vec![c.vpc_cidr.clone()] }, group: gid.clone(), scope: c.region.clone(), ..Default::default() });
            }
            if !c.subnet.is_empty() || !c.subnet_cidr.is_empty() {
                let name = if c.subnet.is_empty() { c.subnet_cidr.clone() } else { c.subnet.clone() };
                let id = format!("i:auto:{}:subnet:{name}", c.provider);
                let node = b.nodes.entry(id.clone()).or_insert_with(|| pb::MapNode { id: id.clone(), kind: "subnet".into(), label: name, server: label.into(), detail: c.subnet_cidr.clone(), addresses: if c.subnet_cidr.is_empty() { vec![] } else { vec![c.subnet_cidr.clone()] }, group: gid.clone(), scope: c.zone.clone(), ..Default::default() });
                node.attrs.push(format!("Server: {} ({})", i.asset.name, c.private));
            }
            if !c.public.is_empty() {
                let id = format!("i:auto:{}:pub", i.asset.id);
                b.nodes.insert(id.clone(), pb::MapNode { id: id.clone(), kind: "pubip".into(), label: c.public.clone(), server: label.into(), detail: format!("public address of {}", i.asset.name), addresses: vec![c.public.clone()], public: true, group: gid, attrs: vec![format!("Maps to: {} ({})", c.private, i.asset.name)], scope: c.zone.clone(), ..Default::default() });
                pub_node[n] = Some(id);
            }
            b.nodes.get_mut(&sid).expect("server").scope = format!("{label} · {}", if c.zone.is_empty() { &c.region } else { &c.zone });
        }
        if !env.gateway.is_empty() {
            facts.push(format!("Default gateway: {}", env.gateway));
        }
        if !env.interfaces.is_empty() {
            facts.push(format!("Interfaces: {}", env.interfaces.iter().map(|x| format!("{} {}", x.name, x.cidr)).collect::<Vec<_>>().join(", ")));
        }
        if !env.routes.is_empty() {
            facts.push(format!("Routes: {}", env.routes.iter().take(12).map(|r| format!("{}{}{}", r.dest, if r.via.is_empty() { String::new() } else { format!(" via {}", r.via) }, if r.dev.is_empty() { String::new() } else { format!(" ({})", r.dev) })).collect::<Vec<_>>().join("; ")));
        }
        if !env.dns.is_empty() {
            facts.push(format!("DNS: {}", env.dns.join(", ")));
        }
        b.nodes.get_mut(&sid).expect("server").attrs.extend(facts);
    }

    // Sources: clouds first, so a cluster's node finds the VM it runs on.
    let mut order: Vec<usize> = (0..sources.len()).collect();
    order.sort_by_key(|i| sources[*i].0.kind == "kubernetes");
    let mut placed: Vec<HashMap<String, At>> = vec![HashMap::new(); sources.len()];
    for si in order.iter().copied() {
        let (src, inv) = &sources[si];
        let group = format!("g:{}", src.id);
        let cluster = src.kind == "kubernetes";
        b.nodes.insert(group.clone(), pb::MapNode { id: group.clone(), kind: if cluster { "cluster".into() } else { "cloud".into() }, label: src.name.clone(), detail: kind_label(&src.kind).into(), group: group.clone(), ..Default::default() });
        for item in &inv.items {
            let machine = item.kind == "vm" || item.kind == "node";
            // One of your monitored servers? Then it is that server, with the cloud's facts added.
            let server = if machine { b.sites.iter().position(|s| item.ips.iter().any(|ip| s.i.topo.ips.contains(ip) || *ip == s.i.asset.host)) } else { None };
            let facts = || {
                let mut f = vec![format!("{}: {} · {}", if item.kind == "node" { "Kubernetes node" } else { kind_label(&src.kind) }, src.name, [item.name.as_str(), item.detail.as_str(), item.scope.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "))];
                f.extend(item.attrs.iter().filter(|a| !a.1.is_empty()).map(|a| format!("{}: {}", a.0, a.1)));
                f
            };
            let at = if let Some(j) = server {
                let id = b.server(j);
                b.nodes.get_mut(&id).expect("server").attrs.extend(facts());
                for ip in &item.ips {
                    b.by_ip.entry(ip.clone()).or_insert(j);
                }
                At::Server(j)
            } else if let Some(vm) = item.ips.iter().find_map(|ip| b.ip_node.get(ip)).filter(|id| item.kind == "node" && b.nodes.get(*id).is_some_and(|n| n.kind == "vm")).cloned() {
                // A cluster node that is a cloud VM timika does not monitor.
                b.nodes.get_mut(&vm).expect("vm").attrs.extend(facts());
                At::Node(vm)
            } else {
                let id = format!("i:{}:{}", src.id, item.id);
                let ports = item.ports.iter().map(|p| pb::MapPort { port: p.0 as u32, proto: p.1.clone(), bind: p.2.clone(), local: false }).collect();
                b.nodes.insert(id.clone(), pb::MapNode { id: id.clone(), kind: item.kind.clone(), label: item.name.clone(), server: src.name.clone(), detail: item.detail.clone(), addresses: item.ips.clone(), ports, state: item.state.clone(), public: item.public, group: group.clone(), attrs: item.attrs.iter().filter(|a| !a.1.is_empty()).map(|a| format!("{}: {}", a.0, a.1)).collect(), scope: item.scope.clone(), ..Default::default() });
                for ip in &item.ips {
                    if !b.by_ip.contains_key(ip) {
                        b.ip_node.entry(ip.clone()).or_insert(id.clone());
                    }
                }
                for p in &item.node_ports {
                    b.node_ports.entry(*p).or_insert(id.clone());
                }
                At::Node(id)
            };
            for r in item.refs.iter().filter(|r| !r.is_empty()) {
                b.refs.entry(r.to_lowercase()).or_insert(at.clone());
            }
            placed[si].insert(item.id.clone(), at);
        }
        notes.extend(inv.notes.iter().map(|n| format!("{}: {n}", src.name)));
    }
    for (si, (src, inv)) in sources.iter().enumerate() {
        for l in &inv.links {
            let Some((from, _)) = b.end(&placed[si], &l.from, l.port, false) else { continue };
            let Some((mut to, mut note)) = b.end(&placed[si], &l.to, l.port, true) else { continue };
            // A load balancer aimed at a node port is aimed at the service behind it.
            if let Some(svc) = b.node_ports.get(&l.port).filter(|_| b.nodes.get(&from).is_some_and(|n| n.kind == "lb")).cloned() {
                note = format!("node port {}", l.port);
                to = svc;
            }
            if from == to {
                continue;
            }
            let e = b.edge(from, to, l.port);
            e.declared = true;
            if e.declared_by.is_empty() {
                e.declared_by = kind_label(&src.kind).into();
            }
            let text = [l.note.as_str(), note.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
            if e.note.is_empty() {
                e.note = text;
            }
        }
    }

    // Outgoing connections first: they know both ends.
    for n in 0..inputs.len() {
        for f in inputs[n].topo.flows.iter().filter(|f| f.out && f.last >= since) {
            let from = if f.owner.is_empty() { b.process(n, &f.process) } else { b.container(n, &f.owner) };
            if !b.nodes.contains_key(&from) {
                continue; // a container that is gone
            }
            let Some((to, note)) = b.resolve(n, &f.owner, &f.peer, f.port) else { continue };
            if to != from {
                let to_process = b.nodes.get(&to).filter(|x| x.kind == "process").map(|x| x.label.clone()).unwrap_or_default();
                b.observe(from, to, f, note, &f.process, &to_process);
            }
        }
    }
    // Incoming ones add what the other side could not tell: clients that are not monitored.
    for n in 0..inputs.len() {
        for f in inputs[n].topo.flows.iter().filter(|f| !f.out && f.last >= since) {
            let (to, note) = if !f.owner.is_empty() {
                (b.container(n, &f.owner), String::new())
            } else if f.process.is_empty() || f.process == "docker-proxy" {
                b.service(n, f.port)
            } else {
                (b.process(n, &f.process), String::new())
            };
            if !b.nodes.contains_key(&to) {
                continue;
            }
            let from = if f.peer == "public" {
                // Through the server's public address, when its cloud told us it has one.
                match pub_node[n].clone() {
                    Some(p) => {
                        let net = b.internet();
                        b.observe(net, p.clone(), f, String::new(), "", "");
                        p
                    }
                    None => b.internet(),
                }
            } else if f.peer == "lb" {
                // Health checks arrive: a load balancer stands in front. Which one, only the cloud's API knows.
                let Some(gid) = auto_card[n].clone() else { continue };
                let id = format!("i:auto:{}:lb", inputs[n].asset.id);
                let name = inputs[n].asset.name.clone();
                b.nodes.entry(id.clone()).or_insert_with(|| pb::MapNode { id: id.clone(), kind: "lb".into(), label: format!("load balancer → {name}"), detail: "inferred from its health checks".into(), group: gid, attrs: vec!["Found by: connections from the cloud's health-check addresses. Add the account under Sources for its name, listeners and other targets.".into()], ..Default::default() });
                id
            } else if let Some(id) = b.ip_node.get(&f.peer).cloned() {
                id
            } else if let Some(j) = b.by_ip.get(&f.peer).copied() {
                // Already known from that server's own outgoing connection?
                let asset = inputs[j].asset.id.clone();
                if j == n || b.edges.iter().any(|(k, e)| k.1 == to && k.2 == f.port as u32 && e.observed && b.nodes.get(&k.0).is_some_and(|x| x.asset == asset)) {
                    continue;
                }
                b.server(j)
            } else {
                b.outside(&f.peer)
            };
            let to_process = if f.process == "docker-proxy" { "" } else { f.process.as_str() };
            b.observe(from, to, f, note, "", to_process);
        }
    }
    // The provider's own API is on the map: it names load balancers and NAT gateways; nothing is guessed.
    let from_api = |provider: &str| sources.iter().find(|(s, _)| s.kind == provider).map(|(s, _)| s.name.clone());
    // No public address, yet it talks to the internet: a NAT gateway carries it.
    for n in 0..inputs.len() {
        let (Some(gid), Some(c)) = (auto_card[n].clone(), inputs[n].topo.env.as_ref().and_then(|e| e.cloud.as_ref())) else { continue };
        if from_api(&c.provider).is_some() || !c.public.is_empty() || !inputs[n].topo.flows.iter().any(|f| f.out && f.last >= since && public(&f.peer)) {
            continue;
        }
        let id = format!("i:auto:{}:nat", gid.trim_start_matches("g:auto:"));
        let gw = inputs[n].topo.env.as_ref().map(|e| e.gateway.clone()).unwrap_or_default();
        b.nodes.entry(id.clone()).or_insert_with(|| pb::MapNode { id: id.clone(), kind: "nat".into(), label: "NAT gateway".into(), detail: "inferred: no public address, but the internet is reached".into(), group: gid, attrs: if gw.is_empty() { vec![] } else { vec![format!("Default gateway: {gw}")] }, ..Default::default() });
        let label = kind_label(&c.provider);
        let from = b.server(n);
        for (a, z) in [(from, id.clone()), (id.clone(), b.internet())] {
            let e = b.edge(a, z, 0);
            e.declared = true;
            e.declared_by = label.into();
            e.note = "inferred: outbound internet traffic of a server without a public address".into();
        }
    }
    // Say what was looked for and not found, so an empty card is an answer too.
    for n in 0..inputs.len() {
        let (Some(gid), Some(c)) = (auto_card[n].clone(), inputs[n].topo.env.as_ref().and_then(|e| e.cloud.as_ref())) else { continue };
        let name = &inputs[n].asset.name;
        if let Some(src) = from_api(&c.provider) {
            if let Some(card) = b.nodes.get_mut(&gid) {
                let line = format!("Load balancers and NAT gateways: from the {} API ({src})", kind_label(&c.provider));
                if !card.attrs.contains(&line) {
                    card.attrs.push(line);
                }
            }
            continue;
        }
        let lb = b.nodes.contains_key(&format!("i:auto:{}:lb", inputs[n].asset.id));
        let lines = [
            if lb {
                format!("Load balancer: in front of {name} (its health checks arrive)")
            } else if c.provider == "aws" {
                format!("Load balancer: can't be told from inside {name} on AWS — add the account under Sources")
            } else {
                format!("Load balancer: none seen in front of {name} (no health checks arrive)")
            },
            if c.public.is_empty() {
                format!("Outbound: {name} has no public address — a NAT gateway (or none) carries its internet traffic")
            } else {
                format!("Outbound: {name} goes out through its own public address {} — no NAT gateway involved", c.public)
            },
        ];
        if let Some(card) = b.nodes.get_mut(&gid) {
            card.attrs.extend(lines);
        }
        if let Some(p) = b.nodes.get_mut(&format!("i:auto:{}:pub", inputs[n].asset.id)) {
            p.attrs.push("Outbound: the server's own traffic to the internet leaves through it as well".into());
        }
    }
    // What nginx is configured to forward to, seen or not.
    for n in 0..inputs.len() {
        for r in &inputs[n].topo.routes {
            let from = if r.slot.is_empty() { b.process(n, "nginx") } else { b.container(n, &r.slot) };
            if !b.nodes.contains_key(&from) {
                continue;
            }
            for backend in &r.backends {
                let Some((host, port)) = backend.rsplit_once(':').and_then(|(h, p)| Some((h.trim_matches(['[', ']']), p.parse::<u16>().ok()?))) else { continue };
                let Some((to, note)) = b.resolve_name(n, &r.slot, host, port) else { continue };
                if to == from {
                    continue;
                }
                let e = b.edge(from.clone(), to, port);
                e.declared = true;
                e.declared_by = "nginx".into();
                e.tls |= r.tls;
                if e.note.is_empty() {
                    e.note = note;
                }
                if !e.sites.contains(&r.site) && e.sites.len() < 20 {
                    e.sites.push(r.site.clone());
                }
            }
        }
    }
    // Everything on a server is drawn in that server's card.
    for n in b.nodes.values_mut().filter(|n| n.group.is_empty() && !n.asset.is_empty()) {
        n.group = format!("s:{}", n.asset);
    }
    let collected = inputs.iter().map(|i| i.topo.time).max().unwrap_or(0);
    pb::NetworkMap { nodes: b.nodes.into_values().collect(), edges: b.edges.into_values().collect(), collected_at: if collected > 0 { iso(collected) } else { String::new() }, notes, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(lines: &[&str]) -> NetRaw {
        let mut r = NetRaw::default();
        for l in lines {
            let (k, v) = l.split_once('=').unwrap();
            r.line(k, v);
        }
        r
    }

    fn input(id: &str, host: &str, raw: &NetRaw, containers: &[(&str, &str, &str)]) -> Input {
        let mut topo = Topo::default();
        topo.absorb(raw, 1000);
        Input {
            asset: Asset { id: id.into(), name: id.into(), host: host.into(), port: 22, tags: vec![], description: String::new(), accounts: vec![], host_key: None, created_at: Utc::now(), updated_at: Utc::now() },
            topo,
            containers: containers.iter().map(|(n, image, ports)| Container { name: n.to_string(), state: "running".into(), image: image.to_string(), ports: ports.to_string(), runtime: "docker".into(), ..Default::default() }).collect(),
            status: "up".into(),
        }
    }

    fn edges(m: &pb::NetworkMap) -> Vec<String> {
        m.edges.iter().map(|e| format!("{} -> {} :{}{}{}", e.from, e.to, e.port, if e.observed { " seen" } else { "" }, if e.declared { " declared" } else { "" })).collect()
    }

    #[test]
    fn reads_the_collector_lines() {
        let r = raw(&["ip=10.0.0.1", "ip=fd00::1", "lsn=tcp|0.0.0.0:22|sshd", "lsn=tcp|[::]:22|sshd", "lsn=udp|127.0.0.53%lo:53|systemd-resolve", "lsn=tcp|*:80|", "con=|10.0.0.1:22|203.0.113.9:51000|sshd", "con=|[::ffff:10.0.0.1]:40000|[::ffff:10.0.0.2]:5432|app; rm", "cin=/web|4242|bridge=172.17.0.2,shop_default=172.20.0.3,|shop", "cls=web|0.0.0.0:80", "ccn=web|172.20.0.3:41000|172.20.0.4:5432|", "deep=1", "con=|bad|worse|x", "lsn=tcp|nonsense|x"]);
        assert_eq!(r.ips, ["10.0.0.1", "fd00::1"]);
        assert_eq!(r.listeners.iter().map(|l| format!("{} {} {} {}", l.proto, l.addr, l.port, l.process)).collect::<Vec<_>>(), ["tcp 0.0.0.0 22 sshd", "tcp :: 22 sshd", "udp 127.0.0.53 53 systemd-resolve", "tcp 0.0.0.0 80 "]);
        assert_eq!(r.conns.len(), 3);
        assert_eq!((r.conns[1].local.clone(), r.conns[1].peer.clone(), r.conns[1].process.as_str()), (("10.0.0.1".into(), 40000), ("10.0.0.2".into(), 5432), "apprm"), "mapped v4 addresses; only safe characters in a name");
        assert_eq!(r.containers, [CNet { name: "web".into(), ips: vec![("bridge".into(), "172.17.0.2".into()), ("shop_default".into(), "172.20.0.3".into())], project: "shop".into(), listens: vec![80] }]);
        assert!(r.deep);
        assert!(public("203.0.113.9") && public("2001:db8::1") && !public("10.1.2.3") && !public("100.64.0.1") && !public("fd00::1") && !public("127.0.0.1"));
    }

    #[test]
    fn flows_keep_direction_and_forget_single_clients() {
        let mut t = Topo::default();
        let r = raw(&[
            "ip=10.0.0.1", "lsn=tcp|0.0.0.0:443|nginx", "lsn=tcp|127.0.0.1:3000|node", "lsn=tcp|0.0.0.0:8080|docker-proxy",
            "con=|10.0.0.1:443|203.0.113.9:51000|nginx", "con=|10.0.0.1:443|198.51.100.7:52000|nginx", "con=|10.0.0.1:443|10.0.0.9:53000|nginx",
            "con=|127.0.0.1:40000|127.0.0.1:3000|nginx", "con=|127.0.0.1:3000|127.0.0.1:40000|node",
            "con=|10.0.0.1:41000|10.0.0.2:5432|node", "con=|10.0.0.1:41001|10.0.0.2:5432|node",
            "con=|172.17.0.1:42000|172.17.0.2:80|docker-proxy", "con=|10.0.0.1:8080|203.0.113.9:54000|docker-proxy",
            "cin=/web|1|bridge=172.17.0.2,|", "cls=web|0.0.0.0:80", "ccn=web|172.17.0.2:80|172.17.0.1:42000|", "ccn=web|172.17.0.2:43000|172.17.0.3:6379|", "ccn=web|127.0.0.1:44000|127.0.0.1:9000|",
            "cin=/cache|2|bridge=172.17.0.3,|", "cls=cache|0.0.0.0:6379", "ccn=cache|172.17.0.3:6379|172.17.0.2:43000|",
        ]);
        assert!(t.absorb(&r, 1000), "new flows are written");
        let mut got: Vec<String> = t.flows.iter().map(|f| format!("{} {}/{} {}:{} x{}", if f.out { "out" } else { "in" }, f.owner, f.process, f.peer, f.port, f.peak)).collect();
        got.sort();
        assert_eq!(
            got,
            [
                "in /docker-proxy public:8080 x1", // the published port: traffic for the container arrives here
                "in /nginx 10.0.0.9:443 x1",       // a private client is kept by address
                "in /nginx public:443 x2",         // internet clients are one flow, counted
                "in cache/ 172.17.0.2:6379 x1",
                "out /nginx 127.0.0.1:3000 x1", // the `in` half of a local connection is not kept twice
                "out /node 10.0.0.2:5432 x2",
                "out web/ 172.17.0.3:6379 x1",
            ]
        );
        assert!(!t.absorb(&r, 1060), "nothing new a minute later");
        assert_eq!(t.flows.iter().map(|f| (f.first, f.last, f.seen)).collect::<HashSet<_>>(), HashSet::from([(1000, 1060, 2)]));
        assert!(t.absorb(&r, 1000 + SAVE_EVERY), "but `last seen` is written now and then");
        assert!(t.absorb(&NetRaw::default(), 1000 + SAVE_EVERY + KEEP_SECS + 60) && t.flows.is_empty(), "old flows are forgotten");
    }

    #[test]
    fn the_map_joins_both_ends() {
        let web = raw(&[
            "ip=10.0.0.1", "lsn=tcp|0.0.0.0:443|nginx", "lsn=tcp|0.0.0.0:22|sshd", "lsn=tcp|0.0.0.0:8080|docker-proxy",
            "con=|10.0.0.1:443|203.0.113.9:51000|nginx", "con=|10.0.0.1:40000|127.0.0.1:8080|nginx", "con=|10.0.0.1:8080|198.51.100.7:52000|docker-proxy",
            "cin=/api|1|bridge=172.17.0.2,|shop", "cls=api|0.0.0.0:3000", "ccn=api|172.17.0.2:41000|10.0.0.2:5432|", "ccn=api|172.17.0.2:41001|52.1.2.3:443|", "ccn=api|172.17.0.2:41002|172.17.0.1:9100|", "deep=1",
            "lsn=tcp|0.0.0.0:9100|node_exporter",
        ]);
        let db = raw(&["ip=10.0.0.2", "lsn=tcp|0.0.0.0:5432|postgres", "lsn=tcp|127.0.0.1:6379|redis-server", "con=|10.0.0.2:5432|10.0.0.1:41000|postgres", "con=|10.0.0.2:5432|10.0.0.77:50000|postgres", "con=|10.0.0.2:45000|10.0.0.1:22|ansible"]);
        let mut inputs = vec![input("web-1", "web-1.internal", &web, &[("api", "acme/api:2", "0.0.0.0:8080->3000/tcp")]), input("db-1", "10.0.0.2", &db, &[])];
        inputs[0].topo.routes = vec![
            Route { slot: String::new(), site: "shop.example.com".into(), ports: vec![443], tls: true, backends: vec!["127.0.0.1:8080".into(), "db-1:9000".into(), "billing.partner.com:443".into()] },
            Route { slot: String::new(), site: "www.example.com".into(), ports: vec![443], tls: true, backends: vec!["localhost:8080".into()] },
        ];
        let m = build(&inputs, 0);
        assert_eq!(
            edges(&m),
            [
                "c:web-1:api -> p:db-1:postgres :5432 seen",           // from inside the container to the database's process
                "c:web-1:api -> p:web-1:node_exporter :9100 seen",     // through the bridge's gateway to the server itself
                "c:web-1:api -> x:52.1.2.3 :443 seen",
                "internet -> c:web-1:api :8080 seen",                  // straight to the published port
                "internet -> p:web-1:nginx :443 seen",
                "p:db-1:ansible -> p:web-1:sshd :22 seen",
                "p:web-1:nginx -> c:web-1:api :8080 seen declared",    // seen, and what the config says
                "p:web-1:nginx -> s:db-1 :9000 declared",              // configured, never seen, nothing listens there
                "p:web-1:nginx -> x:billing.partner.com :443 declared",
                "x:10.0.0.77 -> p:db-1:postgres :5432 seen",           // a client that is not monitored
            ]
        );
        let e = m.edges.iter().find(|e| e.declared && e.observed).unwrap();
        assert_eq!((e.sites.clone(), e.note.as_str(), e.tls, e.from_process.as_str()), (vec!["shop.example.com".to_string(), "www.example.com".to_string()], "published port 8080 → 3000", true, "nginx"));
        let node = |id: &str| m.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!((node("s:web-1").kind.as_str(), node("s:web-1").addresses.clone()), ("server", vec!["10.0.0.1".to_string()]));
        assert_eq!(node("c:web-1:api").ports.iter().map(|p| (p.port, p.bind.as_str())).collect::<Vec<_>>(), [(3000, "published on 8080")]);
        assert_eq!((node("c:web-1:api").project.as_str(), node("c:web-1:api").addresses.clone()), ("shop", vec!["172.17.0.2 (bridge)".to_string()]));
        assert_eq!(node("p:db-1:redis-server").ports.iter().map(|p| (p.port, p.local)).collect::<Vec<_>>(), [(6379, true)], "a listener nobody talks to is still on the map");
        assert!(node("x:52.1.2.3").public && !node("x:10.0.0.77").public && node("internet").public);
        assert!(m.notes.iter().any(|n| n.starts_with("db-1")) == false && m.notes.is_empty(), "{:?}", m.notes);
        // Only what was seen lately.
        assert_eq!(edges(&build(&inputs, 2000)), ["p:web-1:nginx -> c:web-1:api :8080 declared", "p:web-1:nginx -> s:db-1 :9000 declared", "p:web-1:nginx -> x:billing.partner.com :443 declared"]);
        // Someone who may only see db-1: web-1 is just an address.
        let alone = build(&inputs[1..], 0);
        assert_eq!(edges(&alone), ["p:db-1:ansible -> x:10.0.0.1 :22 seen", "x:10.0.0.1 -> p:db-1:postgres :5432 seen", "x:10.0.0.77 -> p:db-1:postgres :5432 seen"]);
    }

    #[test]
    fn clouds_and_clusters_join_the_map() {
        use crate::sources::{Inventory, Item, Source};
        let app = raw(&["ip=10.0.2.7", "lsn=tcp|0.0.0.0:8080|node", "con=|10.0.2.7:40000|10.244.1.7:3000|node", "con=|10.0.2.7:40001|10.96.0.20:80|node", "con=|10.0.2.7:40002|10.0.1.200:443|node"]);
        let inputs = vec![input("app-1", "10.0.2.7", &app, &[])];
        let item = |id: &str, kind: &str, ips: &[&str], refs: &[&str]| Item { id: id.into(), kind: kind.into(), name: id.split([':', '/']).last().unwrap().into(), ips: ips.iter().map(|x| x.to_string()).collect(), refs: refs.iter().map(|x| x.to_string()).collect(), ..Default::default() };
        let mut aws = Inventory { items: vec![item("vm:i-0app", "vm", &["10.0.2.7"], &["i-0app"]), item("vm:i-0other", "vm", &["10.0.3.3"], &["i-0other"]), item("lb:web", "lb", &[], &["arn:lb/web", "Web-1.elb.amazonaws.com"]), item("nat:nat-1", "nat", &["52.9.9.9", "10.0.1.200"], &[])], ..Default::default() };
        aws.items[0].detail = "t3.large".into();
        aws.items[0].attrs = vec![("Instance".into(), "i-0app".into())];
        aws.link("internet", "id:lb:web", 443, "");
        aws.link("id:lb:web", "ref:i-0app", 8080, "target group app · healthy");
        aws.link("id:lb:web", "ref:i-0other", 31080, "target group k8s");
        aws.link("id:vm:i-0app", "id:nat:nat-1", 0, "internet access of subnet-priv");
        aws.link("id:nat:nat-1", "internet", 0, "outbound");
        let mut k8s = Inventory { items: vec![item("node:node-a", "node", &["10.0.3.3"], &["i-0other"]), item("svc:shop/api", "service", &["10.96.0.20"], &[]), item("wl:shop/Deployment/api", "workload", &["10.244.1.7"], &[])], ..Default::default() };
        k8s.items[1].node_ports = vec![31080];
        k8s.link("id:svc:shop/api", "id:wl:shop/Deployment/api", 3000, "service port 80 → 3000");
        k8s.link("ext:web-1.elb.amazonaws.com", "id:svc:shop/api", 80, "load balancer of the service");
        k8s.link("ext:203.0.113.10", "id:svc:shop/api", 80, "load balancer of the service");
        let src = |id: &str, kind: &str| Source { id: id.into(), name: format!("{id}-prod"), kind: kind.into(), ..Default::default() };
        // Given cluster first: the order must not matter.
        let m = build_with(&inputs, &[(src("k", "kubernetes"), k8s), (src("a", "aws"), aws)], 0);
        assert_eq!(
            edges(&m),
            [
                "i:a:lb:web -> i:k:svc:shop/api :80 declared",        // the service's load balancer, found by its DNS name
                "i:a:lb:web -> i:k:svc:shop/api :31080 declared",     // a target on a node port is the service behind it
                "i:a:lb:web -> p:app-1:node :8080 declared",          // an instance that is a monitored server: what listens there
                "i:a:nat:nat-1 -> internet :0 declared",
                "i:k:svc:shop/api -> i:k:wl:shop/Deployment/api :3000 declared",
                "internet -> i:a:lb:web :443 declared",
                "internet -> i:k:svc:shop/api :80 declared",          // an address no source knows: the internet
                "p:app-1:node -> i:a:nat:nat-1 :443 seen",            // a connection to an address that turns out to be the NAT gateway
                "p:app-1:node -> i:k:svc:shop/api :80 seen",          // … a service's cluster address
                "p:app-1:node -> i:k:wl:shop/Deployment/api :3000 seen", // … a pod
                "s:app-1 -> i:a:nat:nat-1 :0 declared",
            ]
        );
        let node = |id: &str| m.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(node("s:app-1").attrs, ["AWS: a-prod · i-0app · t3.large", "Instance: i-0app"], "the server is the VM: one box, with the cloud's facts");
        assert!(!m.nodes.iter().any(|n| n.id == "i:a:vm:i-0app") && !m.nodes.iter().any(|n| n.id == "i:k:node:node-a"), "no second box for the same machine");
        assert_eq!(node("i:a:vm:i-0other").attrs, ["Kubernetes node: k-prod · node-a"], "an unmonitored VM that is a cluster node");
        assert_eq!((node("g:a").kind.as_str(), node("g:a").detail.as_str(), node("g:k").kind.as_str(), node("i:a:lb:web").group.as_str(), node("p:app-1:node").group.as_str(), node("s:app-1").group.as_str()), ("cloud", "AWS", "cluster", "g:a", "s:app-1", "s:app-1"));
        let e = m.edges.iter().find(|e| e.to == "p:app-1:node" && e.declared).unwrap();
        assert_eq!((e.declared_by.as_str(), e.note.as_str()), ("AWS", "target group app · healthy"));
        // Without sources nothing of this is on the map.
        assert_eq!(edges(&build(&inputs, 0)), ["p:app-1:node -> x:10.0.1.200 :443 seen", "p:app-1:node -> x:10.244.1.7 :3000 seen", "p:app-1:node -> x:10.96.0.20 :80 seen"]);
    }

    #[test]
    fn a_server_tells_where_it_is() {
        let e = parse_env("cloud=tencent\nid=ins-abc123\nname=test\ntype=S5.MEDIUM2\nregion=ap-singapore\nzone=ap-singapore-1\nprivate=10.0.0.5\npublic=43.156.34.78\nvpc=vpc-1\nsubnet=subnet-9\nvpc_cidr=10.0.0.0/16\nsubnet_cidr= \nmask=255.255.240.0\npublic_mode=EIP \nimage=img-l8og963d\nbw_out=5\nbw_in=0\ngw=10.0.0.1\ndns=183.60.83.19 183.60.82.98 \nk8s=0\nifc=eth0|10.0.0.5/20\nifc=docker0|172.17.0.1/16\nifc=eth0|fe80::1/64\nroute=default|10.0.0.1|eth0\nroute=172.17.0.0/16||docker0\nroute=$(id)|x|y\nend=1\n");
        let c = e.cloud.clone().unwrap();
        assert_eq!((c.provider.as_str(), c.id.as_str(), c.kind.as_str(), c.zone.as_str(), c.vpc.as_str(), c.private.as_str(), c.public.as_str()), ("tencent", "ins-abc123", "S5.MEDIUM2", "ap-singapore-1", "vpc-1", "10.0.0.5", "43.156.34.78"));
        assert_eq!((e.gateway.as_str(), e.dns.len(), e.k8s), ("10.0.0.1", 2, false));
        assert_eq!((c.vpc_cidr.as_str(), c.subnet_cidr.as_str(), c.public_mode.as_str(), c.image.as_str(), c.bandwidth.as_str()), ("10.0.0.0/16", "10.0.0.0/20", "EIP", "img-l8og963d", "5 Mbit/s out"), "the subnet from the address and its mask");
        assert_eq!(e.interfaces, [Iface { name: "eth0".into(), cidr: "10.0.0.5/20".into() }, Iface { name: "docker0".into(), cidr: "172.17.0.1/16".into() }], "link-local addresses left out");
        assert_eq!(e.routes, [RouteEntry { dest: "default".into(), via: "10.0.0.1".into(), dev: "eth0".into() }, RouteEntry { dest: "172.17.0.0/16".into(), via: String::new(), dev: "docker0".into() }]);
        assert_eq!((network("10.3.19.192", "20"), network("10.3.19.192", "255.255.240.0"), network("x", "20")), ("10.3.16.0/20".to_string(), "10.3.16.0/20".to_string(), String::new()));
        let aws = parse_env("cloud=aws\nid=i-1\ngroups=web  default \nprivate=10.1.2.3\nifc=ens5|10.1.2.3/24\n").cloud.unwrap();
        assert_eq!((aws.groups.clone(), aws.subnet_cidr.as_str()), (vec!["web".to_string(), "default".to_string()], "10.1.2.0/24"), "the subnet from the interface's prefix");
        let g = parse_env("cloud=gcp\nid=123\ntype=projects/1/machineTypes/e2-medium\nzone=projects/1/zones/us-central1-a\nprivate=10.128.0.5\npublic=\nvpc=projects/1/networks/prod\nk8s=1\n").cloud.unwrap();
        assert_eq!((g.kind.as_str(), g.zone.as_str(), g.region.as_str(), g.vpc.as_str(), g.public.as_str()), ("e2-medium", "us-central1-a", "us-central1", "prod", ""));
        let a = parse_env(r#"cloud=azure
azure={"compute":{"vmId":"v-1","name":"web-1","vmSize":"Standard_B2s","location":"southeastasia","zone":"1","resourceGroupName":"prod"},"network":{"interface":[{"ipv4":{"ipAddress":[{"privateIpAddress":"10.1.0.4","publicIpAddress":"20.1.2.3"}],"subnet":[{"address":"10.1.0.0","prefix":"24"}]}}]}}"#).cloud.unwrap();
        assert_eq!((a.id.as_str(), a.zone.as_str(), a.subnet.as_str(), a.private.as_str(), a.public.as_str()), ("v-1", "southeastasia-1", "10.1.0.0/24", "10.1.0.4", "20.1.2.3"));
        assert!(parse_env("cloud=azure\nazure=\ngw=192.168.1.1\n").cloud.is_none(), "Hyper-V at home is not Azure: the metadata service must answer");
        assert!(parse_env("cloud=aws\nid=$(reboot)\n").cloud.unwrap().id == "reboot", "only safe characters are kept");
        assert!(health_check("tencent", "100.64.3.9") && !health_check("aws", "100.64.3.9") && health_check("gcp", "35.191.0.7") && health_check("gcp", "130.211.3.1") && !health_check("gcp", "130.211.4.1") && health_check("azure", "168.63.129.16"));
    }

    #[test]
    fn the_surroundings_of_a_cloud_server() {
        // A VM with a public address, behind a load balancer; another without one, going out through NAT.
        let web = raw(&["ip=10.0.0.5", "lsn=tcp|0.0.0.0:443|nginx", "con=|10.0.0.5:443|203.0.113.9:51000|nginx", "con=|10.0.0.5:443|100.64.1.7:40000|nginx", "con=|10.0.0.5:443|100.64.9.2:40001|nginx", "con=|10.0.0.5:41000|10.0.0.8:8080|nginx"]);
        let app = raw(&["ip=10.0.0.8", "lsn=tcp|0.0.0.0:8080|node", "con=|10.0.0.8:42000|52.1.2.3:443|node"]);
        let cloud = |id: &str, private: &str, public: &str| Env { cloud: Some(CloudInfo { provider: "tencent".into(), id: id.into(), kind: "S5.MEDIUM2".into(), region: "ap-singapore".into(), zone: "ap-singapore-1".into(), vpc: "vpc-1".into(), subnet: "subnet-9".into(), private: private.into(), public: public.into(), vpc_cidr: "10.0.0.0/16".into(), subnet_cidr: "10.0.0.0/20".into(), ..Default::default() }), gateway: "10.0.0.1".into(), dns: vec!["183.60.83.19".into()], ..Default::default() };
        let mut inputs = vec![input("web", "43.156.34.78", &NetRaw::default(), &[]), input("app", "10.0.0.8", &NetRaw::default(), &[])];
        for (i, (r, env)) in [(web, cloud("ins-web", "10.0.0.5", "43.156.34.78")), (app, cloud("ins-app", "10.0.0.8", ""))].into_iter().enumerate() {
            inputs[i].topo.env = Some(env);
            inputs[i].topo.absorb(&r, 1000);
        }
        let m = build(&inputs, 0);
        assert_eq!(
            edges(&m),
            [
                "i:auto:tencent:vpc-1:nat -> internet :0 declared",
                "i:auto:web:lb -> p:web:nginx :443 seen",        // both health-check addresses are one load balancer
                "i:auto:web:pub -> p:web:nginx :443 seen",       // internet traffic arrives through the public address
                "internet -> i:auto:web:pub :443 seen",
                "p:app:node -> x:52.1.2.3 :443 seen",
                "p:web:nginx -> p:app:node :8080 seen",
                "s:app -> i:auto:tencent:vpc-1:nat :0 declared", // no public address, yet it reaches the internet
            ]
        );
        let node = |id: &str| m.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(node("s:web").attrs, ["Tencent Cloud: ins-web · S5.MEDIUM2", "Zone: ap-singapore-1", "VPC: vpc-1 (10.0.0.0/16)", "Subnet: subnet-9 (10.0.0.0/20)", "Private address: 10.0.0.5", "Public address: 43.156.34.78", "Default gateway: 10.0.0.1", "DNS: 183.60.83.19"]);
        assert_eq!((node("i:auto:tencent:vpc:vpc-1").kind.as_str(), node("i:auto:tencent:vpc:vpc-1").detail.as_str(), node("i:auto:tencent:subnet:subnet-9").detail.as_str(), node("i:auto:tencent:subnet:subnet-9").attrs.clone()), ("vpc", "10.0.0.0/16", "10.0.0.0/20", vec!["Server: web (10.0.0.5)".to_string(), "Server: app (10.0.0.8)".to_string()]));
        let card = &node("g:auto:tencent:vpc-1").attrs;
        assert!(card.contains(&"Load balancer: in front of web (its health checks arrive)".to_string()) && card.contains(&"Load balancer: none seen in front of app (no health checks arrive)".to_string()), "{card:?}");
        assert!(card.contains(&"Outbound: web goes out through its own public address 43.156.34.78 — no NAT gateway involved".to_string()) && card.iter().any(|l| l.starts_with("Outbound: app has no public address")), "{card:?}");
        assert_eq!((node("s:web").scope.as_str(), node("g:auto:tencent:vpc-1").label.as_str(), node("g:auto:tencent:vpc-1").detail.as_str()), ("Tencent Cloud · ap-singapore-1", "Tencent Cloud · ap-singapore", "vpc-1"));
        assert_eq!((node("i:auto:web:pub").kind.as_str(), node("i:auto:web:pub").label.as_str(), node("i:auto:web:pub").group.as_str(), node("i:auto:web:lb").detail.as_str()), ("pubip", "43.156.34.78", "g:auto:tencent:vpc-1", "inferred from its health checks"));
        assert!(!m.nodes.iter().any(|n| n.id.starts_with("x:100.64.") || n.id == "x:43.156.34.78"), "no stray addresses");
    }
}
