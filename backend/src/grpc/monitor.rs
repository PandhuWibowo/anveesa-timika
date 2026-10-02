use chrono::Utc;
use tonic::{Request, Status};

use super::convert::ts;
use super::pb::monitor_service_server::MonitorService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::automation::{notify, Notifier, NotifyTarget};
use crate::bastion::{self, Asset};
use crate::error::{AppError, AppResult};
use crate::monitor::{self as mon, LatestRec, Rule, Settings, SystemCfg};
use crate::token::TokenEntry;

fn js<T: serde::Serialize>(v: &T) -> AppResult<Vec<u8>> {
    Ok(serde_json::to_vec(v)?)
}

fn rule_pb(r: &Rule) -> pb::AlertRule {
    pb::AlertRule { metric: r.metric.clone(), threshold: r.threshold, minutes: r.minutes }
}

fn rules_from(r: &[pb::AlertRule]) -> AppResult<Vec<Rule>> {
    let rules: Vec<Rule> = r.iter().map(|x| Rule { metric: x.metric.clone(), threshold: x.threshold, minutes: x.minutes }).collect();
    mon::check_rules(&rules).map_err(AppError::BadRequest)
}

fn system_pb(a: &Asset, cfg: &SystemCfg, rec: &LatestRec) -> pb::System {
    // No fresh reading for a while = nobody is collecting (sealed elsewhere, restarting).
    let stale = (Utc::now() - rec.time).num_seconds() > (mon::interval() as i64 * 3).max(90);
    let n = rec.numbers.as_ref();
    pb::System {
        asset: a.id.clone(),
        name: a.name.clone(),
        host: a.host.clone(),
        tags: a.tags.clone(),
        account: cfg.account.clone(),
        status: if stale && rec.status == "up" { "pending".into() } else { rec.status.clone() },
        since: Some(ts(rec.since)),
        error: rec.error.clone(),
        latest: n.map(|n| pb::Latest {
            cpu: n.cpu,
            mem: n.mem,
            mem_used: n.mem_used,
            mem_total: n.mem_total,
            swap: n.swap,
            disk: n.disk,
            disk_used: n.disk_used,
            disk_total: n.disk_total,
            rx: n.rx,
            tx: n.tx,
            load1: n.load.map(|l| l.0),
            load5: n.load.map(|l| l.1),
            load15: n.load.map(|l| l.2),
            temp: n.temp,
            io_read: n.io_read,
            io_write: n.io_write,
            uptime: n.uptime,
            os: n.os.clone(),
            kernel: n.kernel.clone(),
            cpu_model: n.cpu_model.clone(),
            cpus: n.cpus,
            time: ts(rec.time),
        }),
        spark: rec.spark.iter().map(|v| *v as f64).collect(),
        firing: rec.firing.iter().map(|f| pb::Firing { metric: f.metric.clone(), since: ts(f.since), value: f.value }).collect(),
        containers_running: rec.containers.iter().filter(|c| c.state == "running").count() as u32,
        containers_total: rec.containers.len() as u32,
        failed_units: rec.failed_units.len() as u32,
        last_error: rec.last_error.clone(),
        last_error_at: rec.last_error_at.map(ts),
    }
}

fn container_pb(c: &mon::Container) -> pb::Container {
    pb::Container {
        name: c.name.clone(),
        state: c.state.clone(),
        cpu: c.cpu,
        mem: c.mem,
        image: c.image.clone(),
        status: c.status.clone(),
        ports: c.ports.clone(),
        health: c.health.clone(),
        mem_limit: c.mem_limit,
        rx: c.rx,
        tx: c.tx,
        runtime: c.runtime.clone(),
    }
}

fn settings_pb(s: &Settings) -> pb::MonitorSettings {
    pb::MonitorSettings {
        defaults: s.defaults.iter().map(rule_pb).collect(),
        notifiers: s.notifiers.iter().map(|n| pb::Notifier { id: n.id.clone(), kind: n.kind.clone(), on: n.on.clone(), label: n.label.clone() }).collect(),
        interval: mon::interval() as u32,
    }
}

impl Ctx {
    /// May `me` see this server's metrics? (Admins, or anyone granted an account on it.)
    async fn can_see(&self, me: &TokenEntry, a: &Asset) -> AppResult<bool> {
        Ok(me.is_admin() || !bastion::allowed_accounts(&self.st.core, me, a).await?.is_empty())
    }
}

impl Ctx {
    /// A command about one container on a monitored server, as its monitoring
    /// account, with the container's own runtime (docker / podman). The name
    /// must be one of the containers timika last saw there.
    async fn on_container(&self, asset: &str, name: &str, cmd: impl Fn(&str) -> String, max: usize) -> AppResult<(u32, String)> {
        let core = &self.st.core;
        let cfg: SystemCfg = core.get_json(&mon::system_path(asset)).await?.ok_or_else(|| AppError::NotFound("that server is not monitored".into()))?;
        let rec: LatestRec = core.get_json(&mon::latest_path(asset)).await?.ok_or_else(|| AppError::NotFound("no reading of that server yet".into()))?;
        let found = rec.containers.iter().find(|c| c.name == name).filter(|_| mon::collect::valid_container(name));
        let Some(found) = found else { return Err(AppError::NotFound(format!("container `{name}` on that server"))) };
        let cmd = cmd(&found.runtime);
        let handle = mon::engine::connect(core, &cfg).await.map_err(AppError::Unavailable)?;
        let res = crate::bastion::ssh::exec_out(&handle, &cmd, b"", std::time::Duration::from_secs(60), max).await;
        crate::bastion::ssh::disconnect(&handle).await;
        res.map_err(AppError::Unavailable)
    }
}

#[tonic::async_trait]
impl MonitorService for Ctx {
    async fn list_systems(&self, req: Request<()>) -> Reply<pb::ListSystemsResponse> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let core = &self.st.core;
        let cfgs = mon::systems(core).await?;
        let mut systems = Vec::new();
        let mut available = Vec::new();
        for a in bastion::list_assets(core).await? {
            if !self.can_see(&me, &a).await? {
                continue;
            }
            match cfgs.iter().find(|c| c.asset == a.id) {
                Some(cfg) => {
                    let rec: LatestRec = core.get_json(&mon::latest_path(&a.id)).await?.unwrap_or_else(LatestRec::pending);
                    systems.push(system_pb(&a, cfg, &rec));
                }
                None if me.is_admin() => available.push(pb::Available {
                    asset: a.id.clone(),
                    name: a.name.clone(),
                    host: a.host.clone(),
                    tags: a.tags.clone(),
                    accounts: a.accounts.iter().map(|x| x.username.clone()).collect(),
                }),
                None => {}
            }
        }
        Ok(reply(pb::ListSystemsResponse { systems, available, interval: mon::interval() as u32 }, &me))
    }

    async fn get_system(&self, req: Request<pb::GetSystemRequest>) -> Reply<pb::SystemDetail> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let core = &self.st.core;
        let r = req.get_ref();
        let a = bastion::get_asset(core, &r.asset).await?;
        if !self.can_see(&me, &a).await? {
            return Err(Status::permission_denied(format!("you don't have access to {}", a.name)));
        }
        let cfg: SystemCfg = core.get_json(&mon::system_path(&a.id)).await?.ok_or_else(|| AppError::NotFound(format!("{} is not monitored", a.name)))?;
        let rec: LatestRec = core.get_json(&mon::latest_path(&a.id)).await?.unwrap_or_else(LatestRec::pending);
        let (res, back) = mon::range(&r.range)?;
        let samples = mon::history(core, &a.id, res, back).await?;
        let settings = mon::settings(core).await?;
        // Container samples share the system's timestamps: line them up.
        let index: std::collections::HashMap<i64, usize> = samples.iter().enumerate().map(|(i, s)| (s.0, i)).collect();
        let container_series = mon::container_history(core, &a.id, res, back)
            .await?
            .into_iter()
            .map(|(name, pts)| {
                let mut cols = vec![vec![f64::NAN; samples.len()]; 4];
                for (t, v) in pts {
                    if let Some(i) = index.get(&t) {
                        for (k, col) in cols.iter_mut().enumerate() {
                            if let Some(x) = v[k] {
                                col[*i] = x as f64;
                            }
                        }
                    }
                }
                let mut it = cols.into_iter();
                pb::ContainerSeries { name, cpu: it.next().unwrap_or_default(), mem: it.next().unwrap_or_default(), rx: it.next().unwrap_or_default(), tx: it.next().unwrap_or_default() }
            })
            .collect();
        let series = mon::METRICS
            .iter()
            .enumerate()
            .map(|(i, m)| pb::Series { metric: m.to_string(), values: samples.iter().map(|s| s.1[i].map(|v| v as f64).unwrap_or(f64::NAN)).collect() })
            .collect();
        Ok(reply(
            pb::SystemDetail {
                system: Some(system_pb(&a, &cfg, &rec)),
                disks: rec.disks.iter().map(|d| pb::Disk { device: d.device.clone(), mount: d.mount.clone(), total: d.total, used: d.used }).collect(),
                containers: rec.containers.iter().map(container_pb).collect(),
                container_series,
                failed_unit_names: rec.failed_units.clone(),
                rules: cfg.rules.as_ref().unwrap_or(&settings.defaults).iter().map(rule_pb).collect(),
                default_rules: cfg.rules.is_none(),
                times: samples.iter().map(|s| s.0).collect(),
                series,
                step: mon::step(res),
            },
            &me,
        ))
    }

    async fn set_monitoring(&self, req: Request<pb::SetMonitoringRequest>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let r = req.into_inner();
        crate::audit::target(format!("monitoring {} for {}", if r.enabled { "on" } else { "off" }, r.assets.join(", ")));
        if r.assets.is_empty() || r.assets.len() > 500 {
            return Err(AppError::BadRequest("choose the servers".into()).into());
        }
        if !r.enabled {
            for id in &r.assets {
                mon::remove(core, id).await?;
            }
            return Ok(reply((), &me));
        }
        let mut puts = Vec::new();
        for id in &r.assets {
            let a = bastion::get_asset(core, id).await?;
            let account = if r.account.is_empty() { a.accounts.first().map(|x| x.username.clone()).unwrap_or_default() } else { r.account.clone() };
            if !a.accounts.iter().any(|x| x.username == account) {
                return Err(AppError::BadRequest(format!("{} has no account `{account}`", a.name)).into());
            }
            // Keep rules (and history) when only the account changes.
            let old: Option<SystemCfg> = core.get_json(&mon::system_path(id)).await?;
            let cfg = SystemCfg {
                asset: a.id.clone(),
                account,
                rules: old.as_ref().and_then(|o| o.rules.clone()),
                added_by: old.as_ref().map(|o| o.added_by.clone()).unwrap_or_else(|| me.username.clone().unwrap_or_else(|| me.display_name.clone())),
                added_at: old.map(|o| o.added_at).unwrap_or_else(Utc::now),
            };
            puts.push((mon::system_path(id), js(&cfg)?));
        }
        core.commit(puts, vec![], vec![]).await?;
        Ok(reply((), &me))
    }

    async fn set_system_alerts(&self, req: Request<pb::SystemAlerts>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let r = req.into_inner();
        crate::audit::target(format!("alert rules of {}", r.asset));
        let rules = if r.use_defaults { None } else { Some(rules_from(&r.rules)?) };
        let (cfg, guard) = core.get_json_guarded::<SystemCfg>(&mon::system_path(&r.asset)).await?;
        let mut cfg = cfg.ok_or_else(|| AppError::NotFound("that server is not monitored".into()))?;
        cfg.rules = rules;
        core.commit(vec![(mon::system_path(&r.asset), js(&cfg)?)], vec![], vec![guard]).await?;
        Ok(reply((), &me))
    }

    async fn get_settings(&self, req: Request<()>) -> Reply<pb::MonitorSettings> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        Ok(reply(settings_pb(&mon::settings(&self.st.core).await?), &me))
    }

    async fn save_settings(&self, req: Request<pb::SaveMonitorSettings>) -> Reply<pb::MonitorSettings> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let r = req.into_inner();
        crate::audit::target("monitoring settings (alert defaults, notifications)");
        let defaults = rules_from(&r.defaults)?;
        if r.notifiers.len() > 20 {
            return Err(AppError::BadRequest("at most 20 notifications".into()).into());
        }
        let (old, guard) = core.get_json_guarded::<std::collections::HashMap<String, NotifyTarget>>(mon::SECRETS).await?;
        let old = old.unwrap_or_default();
        let mut targets = std::collections::HashMap::new();
        let mut notifiers = Vec::new();
        for n in r.notifiers {
            let id = if n.id.is_empty() { bastion::random_id(4) } else { n.id.clone() };
            let target = if n.url.trim().is_empty() {
                old.get(&id).cloned().ok_or_else(|| AppError::BadRequest(format!("{}: enter the URL", n.kind)))?
            } else {
                NotifyTarget { url: n.url.trim().to_string(), chat_id: n.chat_id.trim().to_string() }
            };
            let label = notify::check(&n.kind, &target).map_err(AppError::BadRequest)?;
            targets.insert(id.clone(), target);
            notifiers.push(Notifier { id, kind: n.kind, on: vec![], label });
        }
        let s = Settings { defaults, notifiers };
        core.commit(vec![(mon::SETTINGS.into(), js(&s)?), (mon::SECRETS.into(), js(&targets)?)], vec![], vec![guard]).await?;
        Ok(reply(settings_pb(&s), &me))
    }

    async fn list_containers(&self, req: Request<()>) -> Reply<pb::ListContainersResponse> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let core = &self.st.core;
        let cfgs = mon::systems(core).await?;
        let mut containers = Vec::new();
        for a in bastion::list_assets(core).await? {
            if !cfgs.iter().any(|c| c.asset == a.id) || !self.can_see(&me, &a).await? {
                continue;
            }
            let Some(rec) = core.get_json::<LatestRec>(&mon::latest_path(&a.id)).await? else { continue };
            for c in &rec.containers {
                containers.push(pb::ContainerRow { asset: a.id.clone(), system: a.name.clone(), container: Some(container_pb(c)) });
            }
        }
        Ok(reply(pb::ListContainersResponse { containers }, &me))
    }

    async fn container_logs(&self, req: Request<pb::ContainerLogsRequest>) -> Reply<pb::ContainerLogsResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        crate::audit::target(format!("logs of container {} on {}", r.name, r.asset));
        let tail = if r.tail == 0 { 200 } else { r.tail.min(2000) };
        const MAX: usize = 512 * 1024;
        let (_, out) = self.on_container(&r.asset, &r.name, |rt| crate::containers::logs_cmd(rt, &r.name, tail), MAX).await?;
        Ok(reply(pb::ContainerLogsResponse { truncated: out.len() >= MAX, text: out }, &me))
    }

    async fn container_action(&self, req: Request<pb::ContainerActionRequest>) -> Reply<pb::ContainerActionResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        crate::audit::target(format!("{} container {} on {}", r.action, r.name, r.asset));
        if !matches!(r.action.as_str(), "start" | "stop" | "restart") {
            return Err(AppError::BadRequest("a container can be started, stopped or restarted".into()).into());
        }
        let (code, out) = self.on_container(&r.asset, &r.name, |rt| crate::containers::action_cmd(rt, &r.name, &r.action), 16 * 1024).await?;
        Ok(reply(pb::ContainerActionResponse { ok: code == 0, output: out.trim().chars().take(2000).collect() }, &me))
    }

    async fn test_notifier(&self, req: Request<pb::MonitorTestRequest>) -> Reply<pb::TestNotifierResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let n = req.into_inner().notifier.ok_or_else(|| AppError::BadRequest("missing notifier".into()))?;
        crate::audit::target("monitoring (test notification)");
        let target = if n.url.trim().is_empty() {
            mon::secrets(&self.st.core).await?.get(&n.id).cloned().ok_or_else(|| AppError::BadRequest("enter the URL".into()))?
        } else {
            NotifyTarget { url: n.url.trim().to_string(), chat_id: n.chat_id.trim().to_string() }
        };
        notify::check(&n.kind, &target).map_err(AppError::BadRequest)?;
        let res = notify::send(&n.kind, &target, "test", "✓ timika test notification — server alerts will arrive here.", None).await;
        Ok(reply(pb::TestNotifierResponse { ok: res.is_ok(), error: res.err().unwrap_or_default() }, &me))
    }
}
