//! Server monitoring without agents: timika reads each monitored server's
//! metrics over SSH (stored credentials, pinned host key) every
//! `MONITOR_INTERVAL_SECS`, keeps history at three resolutions and raises
//! alerts.
//!
//! Storage (barrier-encrypted):
//!   monitor/systems/<asset>          monitored: account used, own alert rules
//!   monitor/latest/<asset>           status, latest numbers, disks, containers, firing alerts
//!   monitor/d/<asset>/1/<hour>       raw samples of that hour            (kept ~3 h)
//!   monitor/d/<asset>/10/<day>       10-minute averages of that day      (kept ~3 days)
//!   monitor/d/<asset>/60/<month>     hourly averages of that month       (kept MONITOR_RETENTION_DAYS)
//!   monitor/c/<asset>/1/<10 min>     per-container samples (cpu, memory, network) — same
//!   monitor/c/<asset>/10/<day>       three resolutions and retention as above
//!   monitor/c/<asset>/60/<week>
//!   monitor/settings                 default alert rules, notifiers (labels)
//!   monitor/secrets                  notifier targets (URLs, tokens)
//!   monitor/lease                    which instance collects (Redis; Raft: the leader)

pub mod collect;
pub mod engine;

use std::collections::HashMap;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::automation::{Notifier, NotifyTarget};
use crate::core::Core;
use crate::error::{AppError, AppResult};

pub use collect::{Container, Disk};

pub const SYSTEMS: &str = "monitor/systems/";
pub const SETTINGS: &str = "monitor/settings";
pub const SECRETS: &str = "monitor/secrets";

pub fn system_path(asset: &str) -> String {
    format!("{SYSTEMS}{asset}")
}
pub fn latest_path(asset: &str) -> String {
    format!("monitor/latest/{asset}")
}
pub fn data_prefix(asset: &str) -> String {
    format!("monitor/d/{asset}/")
}

/// Seconds between readings.
pub fn interval() -> u64 {
    std::env::var("MONITOR_INTERVAL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(60u64).clamp(1, 3600)
}

/// The values of one sample, in this order.
pub const METRICS: [&str; 10] = ["cpu", "mem", "swap", "disk", "rx", "tx", "load", "temp", "io_read", "io_write"];

/// (unix seconds, values in `METRICS` order) — stored as a JSON array.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Sample(pub i64, pub [Option<f32>; 10]);

/// The average of each metric over `samples` (None where nothing was read).
pub fn average(samples: &[Sample]) -> [Option<f32>; 10] {
    let mut out = [None; 10];
    for (i, o) in out.iter_mut().enumerate() {
        let vals: Vec<f32> = samples.iter().filter_map(|s| s.1[i]).collect();
        if !vals.is_empty() {
            *o = Some(vals.iter().sum::<f32>() / vals.len() as f32);
        }
    }
    out
}

/// The storage bucket a sample at `t` belongs to, for a resolution (1 · 10 · 60).
pub fn bucket(res: u32, t: i64) -> i64 {
    match res {
        1 => t.div_euclid(3600),
        10 => t.div_euclid(86400),
        _ => {
            let d = Utc.timestamp_opt(t, 0).single().unwrap_or_default();
            d.year() as i64 * 12 + d.month0() as i64
        }
    }
}

pub fn bucket_path(asset: &str, res: u32, key: i64) -> String {
    format!("monitor/d/{asset}/{res}/{key}")
}

/// The values of one container sample, in this order.
pub const CMETRICS: [&str; 7] = ["cpu", "mem", "rx", "tx", "io_read", "io_write", "pids"];
/// One container sample: (unix seconds, values in `CMETRICS` order). A Vec, so
/// samples stored before a metric existed (shorter) still read.
pub type CVals = Vec<Option<f32>>;
pub type CPoint = (i64, CVals);
/// Container name → its samples, in one storage bucket.
pub type CBucket = std::collections::BTreeMap<String, Vec<CPoint>>;
/// At most this many containers per server get history (the busiest by name order of `docker ps`).
pub const MAX_CONTAINERS: usize = 40;

/// Container buckets are smaller than the system ones (many series per server):
/// raw per 10 minutes, 10-minute averages per day, hourly averages per week.
pub fn cbucket(res: u32, t: i64) -> i64 {
    match res {
        1 => t.div_euclid(600),
        10 => t.div_euclid(86400),
        _ => t.div_euclid(7 * 86400),
    }
}

pub fn cbucket_path(asset: &str, res: u32, key: i64) -> String {
    format!("monitor/c/{asset}/{res}/{key}")
}

pub fn caverage(points: &[CVals]) -> CVals {
    let mut out = vec![None; CMETRICS.len()];
    for (i, o) in out.iter_mut().enumerate() {
        let vals: Vec<f32> = points.iter().filter_map(|p| p.get(i).copied().flatten()).collect();
        if !vals.is_empty() {
            *o = Some(vals.iter().sum::<f32>() / vals.len() as f32);
        }
    }
    out
}

/// Stored container samples of `asset` for a range, by container.
pub async fn container_history(core: &Core, asset: &str, res: u32, back: i64) -> AppResult<CBucket> {
    let now = Utc::now().timestamp();
    let from = now - back;
    let mut out = CBucket::new();
    for key in cbucket(res, from)..=cbucket(res, now) {
        if let Some(b) = core.get_json::<CBucket>(&cbucket_path(asset, res, key)).await? {
            for (name, pts) in b {
                out.entry(name).or_default().extend(pts.into_iter().filter(|p| p.0 >= from));
            }
        }
    }
    Ok(out)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Rule {
    /// status · cpu · mem · swap · disk · rx · tx · load · temp
    pub metric: String,
    pub threshold: f64,
    pub minutes: u32,
}

pub const RULE_METRICS: [&str; 9] = ["status", "cpu", "mem", "swap", "disk", "rx", "tx", "load", "temp"];

pub fn default_rules() -> Vec<Rule> {
    vec![
        Rule { metric: "status".into(), threshold: 0.0, minutes: 2 },
        Rule { metric: "cpu".into(), threshold: 90.0, minutes: 10 },
        Rule { metric: "mem".into(), threshold: 90.0, minutes: 10 },
        Rule { metric: "disk".into(), threshold: 90.0, minutes: 10 },
    ]
}

pub fn check_rules(rules: &[Rule]) -> Result<Vec<Rule>, String> {
    let mut out: Vec<Rule> = Vec::new();
    for r in rules {
        if !RULE_METRICS.contains(&r.metric.as_str()) {
            return Err(format!("unknown alert metric `{}`", r.metric));
        }
        if out.iter().any(|x| x.metric == r.metric) {
            return Err(format!("{} is listed twice", r.metric));
        }
        if !(1..=1440).contains(&r.minutes) {
            return Err(format!("{}: minutes is 1–1440", r.metric));
        }
        let pct = matches!(r.metric.as_str(), "cpu" | "mem" | "swap" | "disk");
        if r.metric != "status" && (!r.threshold.is_finite() || r.threshold <= 0.0 || pct && r.threshold > 100.0) {
            return Err(format!("{}: the threshold is {}", r.metric, if pct { "1–100 %" } else { "above 0" }));
        }
        out.push(r.clone());
    }
    Ok(out)
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SystemCfg {
    pub asset: String,
    pub account: String,
    /// None = the default rules.
    #[serde(default)]
    pub rules: Option<Vec<Rule>>,
    pub added_by: String,
    pub added_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Firing {
    pub metric: String,
    pub since: DateTime<Utc>,
    #[serde(default)]
    pub value: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Numbers {
    pub cpu: Option<f64>,
    pub mem: Option<f64>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap: Option<f64>,
    pub disk: Option<f64>,
    pub disk_used: u64,
    pub disk_total: u64,
    pub rx: Option<f64>,
    pub tx: Option<f64>,
    pub load: Option<(f64, f64, f64)>,
    pub temp: Option<f64>,
    pub io_read: Option<f64>,
    pub io_write: Option<f64>,
    pub uptime: u64,
    pub os: String,
    pub kernel: String,
    pub cpu_model: String,
    pub cpus: u32,
}

/// What every instance serves: written by the collecting instance each tick.
#[derive(Serialize, Deserialize, Clone)]
pub struct LatestRec {
    /// pending · up · down
    pub status: String,
    pub since: DateTime<Utc>,
    #[serde(default)]
    pub error: Option<String>,
    pub time: DateTime<Utc>,
    #[serde(default)]
    pub numbers: Option<Numbers>,
    #[serde(default)]
    pub disks: Vec<Disk>,
    #[serde(default)]
    pub containers: Vec<Container>,
    /// Container runtimes found on the server (docker, podman).
    #[serde(default)]
    pub runtimes: Vec<String>,
    #[serde(default)]
    pub failed_units: Vec<String>,
    /// CPU % of the last readings.
    #[serde(default)]
    pub spark: Vec<f32>,
    #[serde(default)]
    pub firing: Vec<Firing>,
    /// Status alert already sent for this outage.
    #[serde(default)]
    pub down_notified: bool,
    /// Readings missed in a row (down after two).
    #[serde(default)]
    pub misses: u32,
    /// The last failed reading, kept after recovery.
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_at: Option<DateTime<Utc>>,
    /// The counters of the last reading, so rates survive a restart of timika
    /// (or a change of the collecting instance).
    #[serde(default)]
    pub counters: Option<Counters>,
}

/// What `collect::rates` needs from the previous reading.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Counters {
    pub now: i64,
    pub uptime: u64,
    pub cpu: Option<(u64, u64)>,
    pub net: Option<(u64, u64)>,
    pub io: Option<(u64, u64)>,
}

impl LatestRec {
    pub fn pending() -> Self {
        let now = Utc::now();
        Self { status: "pending".into(), since: now, error: None, time: now, numbers: None, disks: vec![], containers: vec![], runtimes: vec![], failed_units: vec![], spark: vec![], firing: vec![], down_notified: false, misses: 0, last_error: None, last_error_at: None, counters: None }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    pub defaults: Vec<Rule>,
    #[serde(default)]
    pub notifiers: Vec<Notifier>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { defaults: default_rules(), notifiers: vec![] }
    }
}

pub async fn settings(core: &Core) -> AppResult<Settings> {
    Ok(core.get_json(SETTINGS).await?.unwrap_or_default())
}

pub async fn secrets(core: &Core) -> AppResult<HashMap<String, NotifyTarget>> {
    Ok(core.get_json(SECRETS).await?.unwrap_or_default())
}

pub async fn systems(core: &Core) -> AppResult<Vec<SystemCfg>> {
    let mut out = Vec::new();
    for key in core.list(SYSTEMS).await? {
        if let Some(s) = core.get_json::<SystemCfg>(&key).await? {
            out.push(s);
        }
    }
    Ok(out)
}

/// The runtime a container belongs to, from the last reading (docker when unknown).
pub async fn runtime_of(core: &Core, asset: &str, container: &str) -> String {
    let rec: Option<LatestRec> = core.get_json(&latest_path(asset)).await.ok().flatten();
    rec.and_then(|r| r.containers.into_iter().find(|c| c.name == container).map(|c| c.runtime)).unwrap_or_else(|| "docker".into())
}

/// The runtimes on a server, from the last reading (docker when unknown).
pub async fn runtimes(core: &Core, asset: &str) -> Vec<String> {
    let rec: Option<LatestRec> = core.get_json(&latest_path(asset)).await.ok().flatten();
    match rec.map(|r| r.runtimes).filter(|r| !r.is_empty()) {
        Some(r) => r,
        None => vec!["docker".into()],
    }
}

/// Stop monitoring: the system and all of its history.
pub async fn remove(core: &Core, asset: &str) -> AppResult<()> {
    let mut deletes = core.list(&data_prefix(asset)).await?;
    deletes.extend(core.list(&format!("monitor/c/{asset}/")).await?);
    deletes.push(system_path(asset));
    deletes.push(latest_path(asset));
    core.commit(vec![], deletes, vec![]).await
}

/// 1h · 6h · 24h · 7d · 30d · 90d → (resolution, seconds back).
pub fn range(r: &str) -> AppResult<(u32, i64)> {
    Ok(match r {
        "" | "1h" => (1, 3600),
        "6h" => (10, 6 * 3600),
        "24h" => (10, 24 * 3600),
        "7d" => (60, 7 * 86400),
        "30d" => (60, 30 * 86400),
        "90d" => (60, 90 * 86400),
        other => return Err(AppError::BadRequest(format!("unknown range `{other}` (1h · 6h · 24h · 7d · 30d · 90d)"))),
    })
}

/// Seconds between points at a resolution.
pub fn step(res: u32) -> u32 {
    match res {
        1 => interval() as u32,
        10 => 600,
        _ => 3600,
    }
}

/// The stored samples of `asset` for a range, oldest first.
pub async fn history(core: &Core, asset: &str, res: u32, back: i64) -> AppResult<Vec<Sample>> {
    let now = Utc::now().timestamp();
    let from = now - back;
    let (first, last) = (bucket(res, from), bucket(res, now));
    let mut out = Vec::new();
    for key in first..=last {
        if let Some(mut v) = core.get_json::<Vec<Sample>>(&bucket_path(asset, res, key)).await? {
            v.retain(|s| s.0 >= from);
            out.extend(v);
        }
    }
    out.sort_by_key(|s| s.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        let t = 1_790_000_000; // 2026-09-21T…
        assert_eq!(bucket(1, t), t / 3600);
        assert_eq!(bucket(10, t), t / 86400);
        let d = Utc.timestamp_opt(t, 0).unwrap();
        assert_eq!(bucket(60, t), d.year() as i64 * 12 + d.month0() as i64);
        assert_eq!(bucket(60, t) + 1, bucket(60, t + 31 * 86400).min(bucket(60, t) + 1));
        assert_eq!(bucket_path("web", 10, 5), "monitor/d/web/10/5");
    }

    #[test]
    fn averages() {
        let mut a = [None; 10];
        a[0] = Some(10.0);
        a[1] = Some(50.0);
        let mut b = [None; 10];
        b[0] = Some(30.0);
        let avg = average(&[Sample(1, a), Sample(2, b)]);
        assert_eq!((avg[0], avg[1], avg[2]), (Some(20.0), Some(50.0), None));
        assert_eq!(average(&[]), [None; 10]);
        assert_eq!(serde_json::to_string(&Sample(7, a)).unwrap(), "[7,[10.0,50.0,null,null,null,null,null,null,null,null]]");
    }

    #[test]
    fn rules() {
        assert!(check_rules(&default_rules()).is_ok());
        let r = |m: &str, t: f64, min: u32| Rule { metric: m.into(), threshold: t, minutes: min };
        assert!(check_rules(&[r("cpu", 101.0, 5)]).is_err());
        assert!(check_rules(&[r("cpu", 80.0, 0)]).is_err());
        assert!(check_rules(&[r("gpu", 80.0, 5)]).is_err());
        assert!(check_rules(&[r("cpu", 80.0, 5), r("cpu", 90.0, 5)]).is_err());
        assert!(check_rules(&[r("load", 400.0, 5), r("status", 0.0, 1), r("rx", 5e7, 10)]).is_ok());
        assert!(range("2h").is_err());
        assert_eq!(range("24h").unwrap(), (10, 86400));
    }
}
