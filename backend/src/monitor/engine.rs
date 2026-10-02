//! The collection loop. One instance collects (Raft: the leader; Redis: the
//! holder of `monitor/lease`); every instance serves what it stored.
//!
//! Each tick: read every monitored server in parallel over a kept-open SSH
//! connection → a sample → history buckets (raw; 10-minute and hourly averages
//! when their window closes) → alert rules → one commit for everything, then
//! notifications for alerts that started or cleared.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use russh::client::Handle;
use serde::{Deserialize, Serialize};

use super::collect::{self, Raw};
use super::{average, bucket, bucket_path, caverage, cbucket, cbucket_path, interval, latest_path, CBucket, CVals, Firing, LatestRec, Numbers, Rule, Sample, SystemCfg, MAX_CONTAINERS};
use crate::automation::notify;
use crate::bastion::{self, ssh, ssh::Pinned, Secret};
use crate::core::Core;
use crate::error::{AppError, AppResult};

const PARALLEL: usize = 16;
const SPARK: usize = 40;

#[derive(Serialize, Deserialize)]
struct Lease {
    instance: String,
    until: DateTime<Utc>,
}

/// What the collecting instance remembers about one system between ticks.
#[derive(Default)]
struct Mem {
    conn: Option<Handle<Pinned>>,
    prev: Option<Raw>,
    /// Raw samples of the last ~hour and a half (alert windows, roll-ups).
    ring: VecDeque<Sample>,
    hour: (i64, Vec<Sample>),
    day: (i64, Vec<Sample>),
    month: (i64, Vec<Sample>),
    rec: Option<LatestRec>,
    loaded: bool,
    /// Containers: the current raw bucket, and the last ~hour for roll-ups.
    craw: (i64, CBucket),
    cring: VecDeque<(i64, Vec<(String, CVals)>)>,
}

fn human(metric: &str) -> &'static str {
    match metric {
        "cpu" => "CPU",
        "mem" => "Memory",
        "swap" => "Swap",
        "disk" => "Disk",
        "rx" => "Network in",
        "tx" => "Network out",
        "load" => "Load",
        "temp" => "Temperature",
        _ => "Status",
    }
}

pub fn show(metric: &str, v: f64) -> String {
    match metric {
        "cpu" | "mem" | "swap" | "disk" => format!("{v:.0}%"),
        "temp" => format!("{v:.0} °C"),
        "rx" | "tx" => {
            if v >= 1e6 {
                format!("{:.1} MB/s", v / 1e6)
            } else {
                format!("{:.0} kB/s", v / 1e3)
            }
        }
        _ => format!("{v:.2}"),
    }
}

/// The average of `metric` over the last `minutes`, if the readings cover
/// most of that window.
pub fn window_avg(ring: &VecDeque<Sample>, metric: &str, minutes: u32, now: i64, every: u64) -> Option<f64> {
    let i = super::METRICS.iter().position(|m| *m == metric)?;
    let from = now - minutes as i64 * 60;
    let vals: Vec<f32> = ring.iter().filter(|s| s.0 > from).filter_map(|s| s.1[i]).collect();
    let expected = (minutes as u64 * 60 / every.max(1)).max(1) as usize;
    if vals.len() * 10 < expected * 7 {
        return None;
    }
    Some(vals.iter().sum::<f32>() as f64 / vals.len() as f64)
}

/// Apply the rules to a system's state: returns the messages to send.
pub fn evaluate(name: &str, rules: &[Rule], rec: &mut LatestRec, ring: &VecDeque<Sample>, now: DateTime<Utc>, every: u64) -> Vec<String> {
    let mut msgs = Vec::new();
    // Up / down.
    let status_rule = rules.iter().find(|r| r.metric == "status");
    if rec.status == "down" {
        if let Some(r) = status_rule {
            let mins = (now - rec.since).num_seconds() / 60;
            if !rec.down_notified && (now - rec.since).num_seconds() >= r.minutes as i64 * 60 {
                rec.down_notified = true;
                msgs.push(format!("🔴 {name} is down for {mins} min — {}", rec.error.clone().unwrap_or_else(|| "no answer".into())));
            }
        }
    } else if rec.status == "up" && rec.down_notified {
        rec.down_notified = false;
        msgs.push(format!("🟢 {name} is back up"));
    }
    if rec.status != "up" {
        return msgs;
    }
    for r in rules.iter().filter(|r| r.metric != "status") {
        let Some(v) = window_avg(ring, &r.metric, r.minutes, now.timestamp(), every) else { continue };
        let at = rec.firing.iter().position(|f| f.metric == r.metric);
        match (v > r.threshold, at) {
            (true, None) => {
                rec.firing.push(Firing { metric: r.metric.clone(), since: now, value: Some(v) });
                msgs.push(format!("⚠ {name}: {} {} — above {} for {} min", human(&r.metric), show(&r.metric, v), show(&r.metric, r.threshold), r.minutes));
            }
            (true, Some(i)) => rec.firing[i].value = Some(v),
            (false, Some(i)) => {
                rec.firing.remove(i);
                msgs.push(format!("✓ {name}: {} back to {}", human(&r.metric), show(&r.metric, v)));
            }
            (false, None) => {}
        }
    }
    // Rules that were removed stop firing silently.
    rec.firing.retain(|f| rules.iter().any(|r| r.metric == f.metric));
    msgs
}

/// A reading failed. One miss is a gap in the charts; two in a row (or a
/// server that was never read) is "down". The reason is kept either way.
pub fn missed(rec: &mut LatestRec, error: &str, now: DateTime<Utc>) {
    let e: String = error.chars().take(300).collect();
    rec.misses += 1;
    rec.last_error = Some(e.clone());
    rec.last_error_at = Some(now);
    if rec.status != "down" && (rec.misses >= 2 || rec.status == "pending") {
        rec.status = "down".into();
        rec.since = now;
        rec.down_notified = false;
    }
    if rec.status == "down" {
        rec.error = Some(e);
    }
}

pub async fn connect(core: &Core, cfg: &SystemCfg) -> Result<Handle<Pinned>, String> {
    let asset = bastion::get_asset(core, &cfg.asset).await.map_err(|e| e.to_string())?;
    let secret: Secret = core
        .get_json(&bastion::cred_path(&asset.id, &cfg.account))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("the account `{}` no longer exists on this server", cfg.account))?;
    let c = ssh::connect(&asset, &cfg.account, &secret).await?;
    if c.first_seen && !c.host_key.is_empty() {
        let _ = bastion::pin_host_key(core, &asset.id, &c.host_key).await;
    }
    Ok(c.handle)
}

/// One reading; the connection is kept for the next one when it worked. A
/// kept connection that fails (dropped by a firewall, the server restarted
/// sshd) gets one more try on a fresh one.
async fn read(core: Arc<Core>, cfg: SystemCfg, conn: Option<Handle<Pinned>>) -> (String, Option<Handle<Pinned>>, Result<Raw, String>) {
    let reused = conn.as_ref().is_some_and(|h| !h.is_closed());
    let (asset, conn, res) = read_once(core.clone(), cfg.clone(), conn).await;
    match res {
        Err(first) if reused => {
            tracing::info!(server = %asset, "monitor: reading failed on the kept connection ({first}) — retrying on a new one");
            read_once(core, cfg, None).await
        }
        res => (asset, conn, res),
    }
}

async fn read_once(core: Arc<Core>, cfg: SystemCfg, conn: Option<Handle<Pinned>>) -> (String, Option<Handle<Pinned>>, Result<Raw, String>) {
    let handle = match conn.filter(|h| !h.is_closed()) {
        Some(h) => h,
        None => match connect(&core, &cfg).await {
            Ok(h) => h,
            Err(e) => return (cfg.asset, None, Err(e)),
        },
    };
    let limit = Duration::from_secs(interval().clamp(15, 30));
    match ssh::exec_out(&handle, "sh -s", collect::SCRIPT.as_bytes(), limit, 512 * 1024).await {
        Ok((_, out)) => match collect::parse(&out) {
            Ok(raw) => (cfg.asset, Some(handle), Ok(raw)),
            Err(e) => (cfg.asset, Some(handle), Err(e)),
        },
        Err(e) => {
            ssh::disconnect(&handle).await;
            (cfg.asset, None, Err(format!("reading the metrics failed: {e}")))
        }
    }
}

async fn load(core: &Core, asset: &str, m: &mut Mem, now: i64) -> AppResult<()> {
    m.rec = core.get_json(&latest_path(asset)).await?;
    // Rates right after a restart: continue from the stored counters, unless
    // they are too old to mean anything.
    if let Some(c) = m.rec.as_ref().and_then(|r| r.counters).filter(|c| now - c.now < 3600) {
        m.prev = Some(Raw { now: c.now, uptime: c.uptime, cpu: c.cpu, net: c.net, io: c.io, ..Default::default() });
    }
    for (res, slot) in [(1u32, &mut m.hour), (10, &mut m.day), (60, &mut m.month)] {
        let key = bucket(res, now);
        *slot = (key, core.get_json(&bucket_path(asset, res, key)).await?.unwrap_or_default());
    }
    let prev: Vec<Sample> = core.get_json(&bucket_path(asset, 1, bucket(1, now) - 1)).await?.unwrap_or_default();
    m.ring = prev.into_iter().chain(m.hour.1.iter().copied()).filter(|s| s.0 > now - 5400).collect();
    let ckey = cbucket(1, now);
    m.craw = (ckey, core.get_json(&cbucket_path(asset, 1, ckey)).await?.unwrap_or_default());
    // Rebuild the roll-up window from what this bucket holds.
    let mut by_time: std::collections::BTreeMap<i64, Vec<(String, CVals)>> = Default::default();
    for (name, pts) in &m.craw.1 {
        for p in pts {
            by_time.entry(p.0).or_default().push((name.clone(), p.1.clone()));
        }
    }
    m.cring = by_time.into_iter().collect();
    m.loaded = true;
    Ok(())
}

fn numbers(raw: &Raw, r: &collect::Rates) -> Numbers {
    let pct = |used: u64, total: u64| (total > 0).then(|| used as f64 / total as f64 * 100.0);
    let root = raw.disks.iter().find(|d| d.mount == "/").or_else(|| raw.disks.iter().max_by_key(|d| d.total));
    Numbers {
        cpu: r.cpu,
        mem: pct(raw.mem_used, raw.mem_total),
        mem_used: raw.mem_used,
        mem_total: raw.mem_total,
        swap: pct(raw.swap_used, raw.swap_total),
        disk: root.and_then(|d| pct(d.used, d.total)),
        disk_used: root.map(|d| d.used).unwrap_or(0),
        disk_total: root.map(|d| d.total).unwrap_or(0),
        rx: r.rx,
        tx: r.tx,
        load: raw.load,
        temp: raw.temp,
        io_read: r.io_read,
        io_write: r.io_write,
        uptime: raw.uptime,
        os: raw.os.clone(),
        kernel: raw.kernel.clone(),
        cpu_model: raw.cpu_model.clone(),
        cpus: raw.cpus,
    }
}

fn sample(t: i64, n: &Numbers) -> Sample {
    let f = |v: Option<f64>| v.map(|x| x as f32);
    Sample(t, [f(n.cpu), f(n.mem), f(n.swap), f(n.disk), f(n.rx), f(n.tx), f(n.load.map(|l| l.0)), f(n.temp), f(n.io_read), f(n.io_write)])
}

struct Engine {
    core: Arc<Core>,
    mem: HashMap<String, Mem>,
    last_purge: Option<DateTime<Utc>>,
}

impl Engine {
    /// May this instance collect now?
    async fn holds_lease(&self) -> bool {
        if let Some(r) = self.core.storage.raft() {
            return r.is_leader().await;
        }
        let me = self.core.identity.id.clone();
        let Ok((lease, guard)) = self.core.get_json_guarded::<Lease>("monitor/lease").await else { return false };
        let now = Utc::now();
        if lease.as_ref().is_some_and(|l| l.instance != me && l.until > now) {
            return false;
        }
        let mine = Lease { instance: me, until: now + chrono::Duration::seconds(interval() as i64 * 3 + 10) };
        let Ok(bytes) = serde_json::to_vec(&mine) else { return false };
        self.core.commit(vec![("monitor/lease".into(), bytes)], vec![], vec![guard]).await.is_ok()
    }

    async fn tick(&mut self) -> AppResult<()> {
        let core = self.core.clone();
        let systems = super::systems(&core).await?;
        self.mem.retain(|k, _| systems.iter().any(|s| s.asset == *k));
        if systems.is_empty() {
            return Ok(());
        }
        let settings = super::settings(&core).await?;
        let every = interval();
        let now = Utc::now();
        let t = now.timestamp();

        // Read everything, a few at a time.
        let gate = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
        let mut set = tokio::task::JoinSet::new();
        for cfg in &systems {
            let m = self.mem.entry(cfg.asset.clone()).or_default();
            if !m.loaded {
                load(&core, &cfg.asset, m, t).await?;
            }
            let (core, cfg, conn, gate) = (core.clone(), cfg.clone(), m.conn.take(), gate.clone());
            set.spawn(async move {
                let _permit = gate.acquire_owned().await;
                read(core, cfg, conn).await
            });
        }
        let mut results = HashMap::new();
        while let Some(r) = set.join_next().await {
            if let Ok((asset, conn, res)) = r {
                results.insert(asset, (conn, res));
            }
        }

        let mut puts: Vec<(String, Vec<u8>)> = Vec::new();
        let mut gone: Vec<String> = Vec::new();
        let mut messages: Vec<String> = Vec::new();
        for cfg in &systems {
            let Some((conn, res)) = results.remove(&cfg.asset) else { continue };
            let name = match bastion::get_asset(&core, &cfg.asset).await {
                Ok(a) => a.name,
                Err(AppError::NotFound(_)) => {
                    gone.push(cfg.asset.clone());
                    continue;
                }
                Err(e) => return Err(e),
            };
            let m = self.mem.get_mut(&cfg.asset).expect("mem entry");
            m.conn = conn;
            let mut rec = m.rec.take().unwrap_or_else(LatestRec::pending);
            rec.time = now;
            match res {
                Ok(mut raw) => {
                    collect::container_rates(m.prev.as_ref(), &mut raw);
                    let n = numbers(&raw, &collect::rates(m.prev.as_ref(), &raw));
                    let s = sample(t, &n);
                    if rec.status != "up" {
                        rec.status = "up".into();
                        rec.since = now;
                    }
                    rec.error = None;
                    rec.misses = 0;
                    if let Some(c) = n.cpu {
                        rec.spark.push(c as f32);
                        if rec.spark.len() > SPARK {
                            rec.spark.remove(0);
                        }
                    }
                    rec.numbers = Some(n);
                    rec.disks = raw.disks.clone();
                    rec.containers = raw.containers.clone();
                    rec.runtimes = raw.runtimes.clone();
                    rec.failed_units = raw.failed_units.clone();
                    rec.counters = Some(super::Counters { now: raw.now, uptime: raw.uptime, cpu: raw.cpu, net: raw.net, io: raw.io });
                    m.prev = Some(raw);

                    // History: raw now; 10-minute and hourly averages when their window closes.
                    let last = m.ring.back().map(|x| x.0);
                    if let Some(last) = last {
                        if last.div_euclid(600) != t.div_euclid(600) {
                            let w = last.div_euclid(600) * 600;
                            let win: Vec<Sample> = m.ring.iter().filter(|x| x.0 >= w && x.0 < w + 600).copied().collect();
                            if !win.is_empty() {
                                let key = bucket(10, w);
                                if m.day.0 != key {
                                    m.day = (key, core.get_json(&bucket_path(&cfg.asset, 10, key)).await?.unwrap_or_default());
                                }
                                m.day.1.push(Sample(w, average(&win)));
                                puts.push((bucket_path(&cfg.asset, 10, key), serde_json::to_vec(&m.day.1)?));
                            }
                        }
                        if last.div_euclid(3600) != t.div_euclid(3600) {
                            let w = last.div_euclid(3600) * 3600;
                            let win: Vec<Sample> = m.ring.iter().filter(|x| x.0 >= w && x.0 < w + 3600).copied().collect();
                            if !win.is_empty() {
                                let key = bucket(60, w);
                                if m.month.0 != key {
                                    m.month = (key, core.get_json(&bucket_path(&cfg.asset, 60, key)).await?.unwrap_or_default());
                                }
                                m.month.1.push(Sample(w, average(&win)));
                                puts.push((bucket_path(&cfg.asset, 60, key), serde_json::to_vec(&m.month.1)?));
                            }
                        }
                    }
                    // Containers: the same three resolutions, in their own buckets.
                    let cpoints: Vec<(String, CVals)> = m
                        .prev
                        .as_ref()
                        .map(|r| &r.containers)
                        .into_iter()
                        .flatten()
                        .filter(|c| c.state == "running")
                        .take(MAX_CONTAINERS)
                        .map(|c| {
                            let f = |v: Option<f64>| v.map(|x| x as f32);
                            (c.name.clone(), vec![f(c.cpu), Some(c.mem as f32), f(c.rx), f(c.tx), f(c.io_read), f(c.io_write), c.pids.map(|p| p as f32)])
                        })
                        .collect();
                    if let Some(last) = last {
                        for (res, span) in [(10u32, 600i64), (60, 3600)] {
                            if last.div_euclid(span) == t.div_euclid(span) {
                                continue;
                            }
                            let w = last.div_euclid(span) * span;
                            let mut per: std::collections::BTreeMap<&str, Vec<CVals>> = Default::default();
                            for (ts, pts) in m.cring.iter().filter(|(ts, _)| *ts >= w && *ts < w + span) {
                                let _ = ts;
                                for (name, p) in pts {
                                    per.entry(name.as_str()).or_default().push(p.clone());
                                }
                            }
                            if per.is_empty() {
                                continue;
                            }
                            let path = cbucket_path(&cfg.asset, res, cbucket(res, w));
                            let mut b: CBucket = core.get_json(&path).await?.unwrap_or_default();
                            for (name, pts) in per {
                                b.entry(name.to_string()).or_default().push((w, caverage(&pts)));
                            }
                            puts.push((path, serde_json::to_vec(&b)?));
                        }
                    }
                    if !cpoints.is_empty() {
                        let ckey = cbucket(1, t);
                        if m.craw.0 != ckey {
                            m.craw = (ckey, CBucket::new());
                        }
                        for (name, p) in &cpoints {
                            m.craw.1.entry(name.clone()).or_default().push((t, p.clone()));
                        }
                        puts.push((cbucket_path(&cfg.asset, 1, ckey), serde_json::to_vec(&m.craw.1)?));
                    }
                    m.cring.push_back((t, cpoints));
                    while m.cring.front().is_some_and(|x| x.0 < t - 4200) {
                        m.cring.pop_front();
                    }

                    let key = bucket(1, t);
                    if m.hour.0 != key {
                        m.hour = (key, Vec::new());
                    }
                    m.hour.1.push(s);
                    puts.push((bucket_path(&cfg.asset, 1, key), serde_json::to_vec(&m.hour.1)?));
                    m.ring.push_back(s);
                    while m.ring.front().is_some_and(|x| x.0 < t - 5400) {
                        m.ring.pop_front();
                    }
                }
                Err(e) => {
                    tracing::warn!(server = %name, "monitor: reading failed: {e}");
                    missed(&mut rec, &e, now);
                }
            }
            let rules = cfg.rules.as_deref().unwrap_or(&settings.defaults);
            messages.extend(evaluate(&name, rules, &mut rec, &m.ring, now, every));
            puts.push((latest_path(&cfg.asset), serde_json::to_vec(&rec)?));
            m.rec = Some(rec);
        }
        core.commit(puts, vec![], vec![]).await?;
        for asset in gone {
            self.mem.remove(&asset);
            let _ = super::remove(&core, &asset).await;
        }

        if !messages.is_empty() && !settings.notifiers.is_empty() {
            let targets = super::secrets(&core).await.unwrap_or_default();
            let link = std::env::var("PUBLIC_URL").ok().map(|u| format!("\n{}/#/monitoring", u.trim().trim_end_matches('/'))).unwrap_or_default();
            let sends: Vec<_> = settings.notifiers.iter().filter_map(|n| targets.get(&n.id).map(|t| (n.kind.clone(), t.clone()))).collect();
            tokio::spawn(async move {
                for text in messages {
                    for (kind, target) in &sends {
                        if let Err(e) = notify::send(kind, target, "monitor", &format!("{text}{link}"), None).await {
                            tracing::warn!("monitor: notification failed: {e}");
                        }
                    }
                }
            });
        }

        if self.last_purge.is_none_or(|p| (now - p).num_seconds() > 3600) {
            self.last_purge = Some(now);
            self.purge(&systems, t).await?;
        }
        Ok(())
    }

    /// Drop history past its retention.
    async fn purge(&self, systems: &[SystemCfg], t: i64) -> AppResult<()> {
        let days: i64 = std::env::var("MONITOR_RETENTION_DAYS").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
        let mut deletes = Vec::new();
        for s in systems {
            for key in self.core.list(&super::data_prefix(&s.asset)).await? {
                let mut parts = key.rsplit('/');
                let (Some(b), Some(res)) = (parts.next().and_then(|x| x.parse::<i64>().ok()), parts.next()) else { continue };
                let old = match res {
                    "1" => b < bucket(1, t - 3 * 3600),
                    "10" => b < bucket(10, t - 3 * 86400),
                    _ => b < bucket(60, t - (days + 31) * 86400),
                };
                if old {
                    deletes.push(key);
                }
            }
        }
        for s in systems {
            for key in self.core.list(&format!("monitor/c/{}/", s.asset)).await? {
                let mut parts = key.rsplit('/');
                let (Some(b), Some(res)) = (parts.next().and_then(|x| x.parse::<i64>().ok()), parts.next()) else { continue };
                let old = match res {
                    "1" => b < cbucket(1, t - 3 * 3600),
                    "10" => b < cbucket(10, t - 3 * 86400),
                    _ => b < cbucket(60, t - (days + 8) * 86400),
                };
                if old {
                    deletes.push(key);
                }
            }
        }
        if !deletes.is_empty() {
            self.core.commit(vec![], deletes, vec![]).await?;
        }
        Ok(())
    }
}

pub fn spawn(core: Arc<Core>) {
    tokio::spawn(async move {
        let mut e = Engine { core, mem: HashMap::new(), last_purge: None };
        loop {
            let started = std::time::Instant::now();
            if !e.core.is_sealed().await && e.holds_lease().await {
                if let Err(err) = e.tick().await {
                    tracing::warn!("monitor: tick failed: {err}");
                }
            } else {
                // Not collecting: drop connections and state (another instance has them).
                for (_, m) in e.mem.drain() {
                    if let Some(h) = m.conn {
                        ssh::disconnect(&h).await;
                    }
                }
            }
            let every = Duration::from_secs(interval());
            tokio::time::sleep(every.saturating_sub(started.elapsed()).max(Duration::from_millis(200))).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(vals: &[f32], every: i64, end: i64) -> VecDeque<Sample> {
        vals.iter().enumerate().map(|(i, v)| {
            let mut a = [None; 10];
            a[0] = Some(*v);
            Sample(end - (vals.len() - 1 - i) as i64 * every, a)
        }).collect()
    }

    #[test]
    fn window_average_needs_coverage() {
        let now = 1_790_000_000;
        let r = ring(&[80.0, 90.0, 100.0, 95.0, 85.0], 60, now);
        assert_eq!(window_avg(&r, "cpu", 5, now, 60), Some(90.0));
        assert_eq!(window_avg(&r, "cpu", 10, now, 60), None, "only half the window was read");
        assert_eq!(window_avg(&r, "mem", 5, now, 60), None, "no readings of that metric");
        assert_eq!(window_avg(&r, "cpu", 2, now, 60), Some(90.0));
    }

    #[test]
    fn alerts_fire_once_and_clear() {
        let now = Utc::now();
        let t = now.timestamp();
        let rules = vec![Rule { metric: "cpu".into(), threshold: 90.0, minutes: 3 }, Rule { metric: "status".into(), threshold: 0.0, minutes: 2 }];
        let mut rec = LatestRec::pending();
        rec.status = "up".into();
        let hot = ring(&[95.0, 96.0, 97.0], 60, t);
        let m = evaluate("web-01", &rules, &mut rec, &hot, now, 60);
        assert_eq!(m, ["⚠ web-01: CPU 96% — above 90% for 3 min"]);
        assert_eq!(rec.firing.len(), 1);
        assert!(evaluate("web-01", &rules, &mut rec, &hot, now, 60).is_empty(), "not repeated while it lasts");
        let cool = ring(&[40.0, 30.0, 20.0], 60, t);
        assert_eq!(evaluate("web-01", &rules, &mut rec, &cool, now, 60), ["✓ web-01: CPU back to 30%"]);
        assert!(rec.firing.is_empty());

        // Down: announced once after the rule's minutes, then "back up".
        rec.status = "down".into();
        rec.since = now - chrono::Duration::seconds(60);
        rec.error = Some("could not connect".into());
        assert!(evaluate("web-01", &rules, &mut rec, &cool, now, 60).is_empty(), "not yet 2 minutes");
        rec.since = now - chrono::Duration::seconds(130);
        assert_eq!(evaluate("web-01", &rules, &mut rec, &cool, now, 60), ["🔴 web-01 is down for 2 min — could not connect"]);
        assert!(evaluate("web-01", &rules, &mut rec, &cool, now, 60).is_empty());
        rec.status = "up".into();
        assert_eq!(evaluate("web-01", &rules, &mut rec, &cool, now, 60)[0], "🟢 web-01 is back up");
        // A short blip (never announced) recovers silently.
        rec.status = "up".into();
        assert!(evaluate("web-01", &rules, &mut rec, &ring(&[], 60, t), now, 60).is_empty());
    }

    #[test]
    fn one_miss_is_a_gap_two_are_down() {
        let now = Utc::now();
        let mut rec = LatestRec::pending();
        rec.status = "up".into();
        let up_since = rec.since;
        missed(&mut rec, "connection reset", now);
        assert_eq!((rec.status.as_str(), rec.misses, rec.error.clone(), rec.since), ("up", 1, None, up_since), "still up after one miss");
        assert_eq!(rec.last_error.as_deref(), Some("connection reset"), "but the reason is kept");
        missed(&mut rec, "did not answer", now);
        assert_eq!((rec.status.as_str(), rec.error.as_deref(), rec.since), ("down", Some("did not answer"), now));
        missed(&mut rec, "still nothing", now + chrono::Duration::seconds(60));
        assert_eq!((rec.since, rec.error.as_deref()), (now, Some("still nothing")), "down since the second miss; latest reason shown");
        // A server never read yet is down on its first failure (wrong OS, bad account).
        let mut fresh = LatestRec::pending();
        missed(&mut fresh, "only Linux servers can be monitored", now);
        assert_eq!(fresh.status, "down");
    }

    #[test]
    fn values_for_people() {
        assert_eq!(show("cpu", 93.6), "94%");
        assert_eq!(show("rx", 2_500_000.0), "2.5 MB/s");
        assert_eq!(show("tx", 40_000.0), "40 kB/s");
        assert_eq!(show("load", 1.5), "1.50");
        assert_eq!(show("temp", 71.2), "71 °C");
    }
}
