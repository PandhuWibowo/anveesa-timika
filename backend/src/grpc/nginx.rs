use std::time::Duration;

use chrono::Utc;
use russh::client::Handle;
use tonic::Request;

use super::convert::ts;
use super::pb::nginx_service_server::NginxService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::bastion::{self, ssh, ssh::Pinned};
use crate::containers as dk;
use crate::error::{AppError, AppResult};
use crate::monitor::{self as mon, LatestRec, SystemCfg};
use crate::nginx::{self as ngx, Switch, Target};
use crate::token::TokenEntry;

fn who_name(me: &TokenEntry) -> String {
    me.username.clone().unwrap_or_else(|| me.display_name.clone())
}

async fn exec(h: &Handle<Pinned>, cmd: &str, stdin: &[u8], secs: u64, max: usize) -> AppResult<(u32, String)> {
    ssh::exec_out(h, cmd, stdin, Duration::from_secs(secs), max).await.map_err(AppError::Unavailable)
}

/// One nginx, with the SSH connection to its server (as the monitoring account).
struct Open {
    system: String,
    target: Target,
    handle: Handle<Pinned>,
}

impl Ctx {
    async fn nginx(&self, asset: &str, container: &str) -> AppResult<Open> {
        let core = &self.st.core;
        let cfg: SystemCfg = core
            .get_json(&mon::system_path(asset))
            .await?
            .ok_or_else(|| AppError::NotFound("that server is not monitored — add it under Monitoring → Systems first".into()))?;
        let target = if container.is_empty() {
            Target { container: String::new(), runtime: String::new() }
        } else {
            dk::check_name(container)?;
            let rec: LatestRec = core.get_json(&mon::latest_path(asset)).await?.unwrap_or_else(LatestRec::pending);
            let c = rec.containers.iter().find(|c| c.name == container).ok_or_else(|| AppError::NotFound(format!("container `{container}`")))?;
            Target { container: c.name.clone(), runtime: c.runtime.clone() }
        };
        let system = bastion::get_asset(core, asset).await?.name;
        let handle = mon::engine::connect(core, &cfg).await.map_err(AppError::Unavailable)?;
        Ok(Open { system, target, handle })
    }
}

impl Open {
    async fn run(&self, cmd: &str, stdin: &[u8], secs: u64, max: usize) -> AppResult<String> {
        let (code, out) = exec(&self.handle, cmd, stdin, secs, max).await?;
        ngx::expect_ran(code, &out, &self.target)?;
        Ok(out)
    }
    async fn close(self) {
        ssh::disconnect(&self.handle).await;
    }
    /// The file as it is on the server now (None: not there).
    async fn read(&self, path: &str) -> AppResult<Option<String>> {
        if ngx::is_key_material(path) {
            return Err(AppError::BadRequest("certificates and keys are not opened here — only configuration files".into()));
        }
        let (code, out) = exec(&self.handle, &self.target.read_cmd(path), b"", 20, ngx::MAX_FILE + 1).await?;
        if code == 6 {
            return Ok(None);
        }
        ngx::expect_ran(code, &out, &self.target)?;
        if out.contains("PRIVATE KEY-----") {
            return Err(AppError::BadRequest("this file holds a private key — it is not opened here".into()));
        }
        if out.len() > ngx::MAX_FILE {
            return Err(AppError::BadRequest(format!("this file is larger than {} KiB — edit it in a terminal", ngx::MAX_FILE / 1024)));
        }
        Ok(Some(out))
    }
}

fn label(asset: &str, container: &str) -> String {
    if container.is_empty() { format!("nginx on {asset}") } else { format!("nginx in {container} on {asset}") }
}

#[tonic::async_trait]
impl NginxService for Ctx {
    async fn list_instances(&self, req: Request<()>) -> Reply<pb::ListNginxResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let cfgs = mon::systems(core).await?;
        let mut instances = Vec::new();
        for a in bastion::list_assets(core).await? {
            if !cfgs.iter().any(|c| c.asset == a.id) {
                continue;
            }
            let Some(rec) = core.get_json::<LatestRec>(&mon::latest_path(&a.id)).await? else { continue };
            if let Some((version, running)) = &rec.nginx {
                instances.push(pb::NginxSummary { asset: a.id.clone(), system: a.name.clone(), version: version.clone(), running: *running, ..Default::default() });
            }
            for c in rec.containers.iter().filter(|c| ngx::is_nginx_image(&c.image)) {
                instances.push(pb::NginxSummary { asset: a.id.clone(), system: a.name.clone(), container: c.name.clone(), runtime: c.runtime.clone(), running: c.state == "running", image: c.image.clone(), ..Default::default() });
            }
        }
        Ok(reply(pb::ListNginxResponse { instances, monitored: cfgs.len() as u32 }, &me))
    }

    async fn get_instance(&self, req: Request<pb::NginxRef>) -> Reply<pb::NginxInstance> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        crate::audit::target(label(&r.asset, &r.container));
        let o = self.nginx(&r.asset, &r.container).await?;
        let res = async {
            let out = o.run(&o.target.overview_cmd(), b"", 40, 2 * 1024 * 1024).await?;
            let mut inst = ngx::overview(&out);
            if !inst.certificates.is_empty() {
                let paths: Vec<String> = inst.certificates.iter().map(|c| c.path.clone()).filter(|p| ngx::clean_path(p).is_ok()).take(40).collect();
                // Best effort: a server without openssl still shows its sites.
                if let Ok((_, out)) = exec(&o.handle, &o.target.certs_cmd(&paths), b"", 30, 256 * 1024).await {
                    ngx::certs(&out, &mut inst.certificates, Utc::now());
                }
            }
            inst.system = o.system.clone();
            inst.runtime = o.target.runtime.clone();
            Ok::<_, AppError>(inst)
        }
        .await;
        o.close().await;
        Ok(reply(res?, &me))
    }

    async fn read_file(&self, req: Request<pb::NginxPath>) -> Reply<pb::NginxFile> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        let path = ngx::clean_path(&r.path)?;
        crate::audit::target(format!("{path} of {}", label(&r.asset, &r.container)));
        let o = self.nginx(&r.asset, &r.container).await?;
        let res = o.read(&path).await;
        let target = o.target.clone();
        o.close().await;
        let content = res?.ok_or_else(|| AppError::NotFound(format!("{path} is not there (any more)")))?;
        let versions = ngx::history(&self.st.core, &r.asset, &target, &path).await?.versions.len() as u32;
        Ok(reply(pb::NginxFile { rev: ngx::rev(&content), path, content, versions }, &me))
    }

    async fn save_file(&self, req: Request<pb::SaveNginxFile>) -> Reply<pb::NginxApply> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        let path = ngx::clean_path(&r.path)?;
        let content = ngx::clean_content(&r.content)?;
        if ngx::is_key_material(&path) || content.contains("PRIVATE KEY-----") {
            return Err(AppError::BadRequest("certificates and keys are not written here — only configuration files".into()).into());
        }
        crate::audit::target(format!("{path} of {}", label(&r.asset, &r.container)));
        let create = r.rev.is_empty();
        let link = if create && r.enable { ngx::enabled_link(&path).unwrap_or_default() } else { String::new() };
        let o = self.nginx(&r.asset, &r.container).await?;
        let res = async {
            // What is there now: to notice a change made meanwhile, and for the history.
            let before = if create { None } else { Some(o.read(&path).await?.ok_or_else(|| AppError::NotFound(format!("{path} is not there (any more)")))?) };
            if before.as_ref().is_some_and(|b| ngx::rev(b) != r.rev) {
                return Err(AppError::Conflict("this file changed on the server since you opened it — reload it and apply your change again".into()));
            }
            let out = o.run(&o.target.save_cmd(&path, create, &link), content.as_bytes(), 60, 64 * 1024).await?;
            Ok((before, ngx::applied(&out)))
        }
        .await;
        let target = o.target.clone();
        o.close().await;
        let (before, mut done) = res?;
        if done.ok {
            let mut add = Vec::new();
            if let Some(b) = before {
                add.push((b, String::new(), "as found on the server".to_string()));
            }
            add.push((content.clone(), who_name(&me), r.note.chars().take(200).collect()));
            ngx::remember(&self.st.core, &r.asset, &target, &path, add).await?;
            done.rev = ngx::rev(&content);
        }
        done.path = path;
        Ok(reply(done, &me))
    }

    async fn set_enabled(&self, req: Request<pb::SetNginxEnabled>) -> Reply<pb::NginxApply> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        let path = ngx::clean_path(&r.path)?;
        let sw = Switch::new(&path, r.enabled)?;
        crate::audit::target(format!("{} {path} of {}", if r.enabled { "enable" } else { "disable" }, label(&r.asset, &r.container)));
        let o = self.nginx(&r.asset, &r.container).await?;
        let res = o.run(&o.target.switch_cmd(&sw), b"", 60, 64 * 1024).await;
        o.close().await;
        let mut done = ngx::applied(&res?);
        done.path = if done.ok { sw.path().to_string() } else { path };
        Ok(reply(done, &me))
    }

    async fn delete_file(&self, req: Request<pb::NginxPath>) -> Reply<pb::NginxApply> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        let path = ngx::clean_path(&r.path)?;
        crate::audit::target(format!("delete {path} of {}", label(&r.asset, &r.container)));
        let o = self.nginx(&r.asset, &r.container).await?;
        let res = async {
            let before = o.read(&path).await.ok().flatten();
            let out = o.run(&o.target.delete_cmd(&path), b"", 60, 64 * 1024).await?;
            Ok::<_, AppError>((before, ngx::applied(&out)))
        }
        .await;
        let target = o.target.clone();
        o.close().await;
        let (before, mut done) = res?;
        // Kept in the history, so a deleted file can be brought back.
        if let (true, Some(b)) = (done.ok, before) {
            ngx::remember(&self.st.core, &r.asset, &target, &path, vec![(b, who_name(&me), "before it was deleted".into())]).await?;
        }
        done.path = path;
        Ok(reply(done, &me))
    }

    async fn reload(&self, req: Request<pb::NginxRef>) -> Reply<pb::NginxApply> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        crate::audit::target(format!("reload {}", label(&r.asset, &r.container)));
        let o = self.nginx(&r.asset, &r.container).await?;
        let res = o.run(&o.target.reload_cmd(), b"", 60, 64 * 1024).await;
        o.close().await;
        Ok(reply(ngx::applied(&res?), &me))
    }

    async fn list_versions(&self, req: Request<pb::NginxPath>) -> Reply<pb::NginxVersions> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        let path = ngx::clean_path(&r.path)?;
        if !r.container.is_empty() {
            dk::check_name(&r.container)?;
        }
        let t = Target { container: r.container.clone(), runtime: String::new() };
        let h = ngx::history(&self.st.core, &r.asset, &t, &path).await?;
        let versions = h.versions.iter().rev().map(|v| pb::NginxVersion { id: v.id, at: ts(v.at), by: v.by.clone(), note: v.note.clone(), size: v.content.len() as u32 }).collect();
        Ok(reply(pb::NginxVersions { versions }, &me))
    }

    async fn get_version(&self, req: Request<pb::NginxVersionRef>) -> Reply<pb::NginxFile> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        let path = ngx::clean_path(&r.path)?;
        if !r.container.is_empty() {
            dk::check_name(&r.container)?;
        }
        crate::audit::target(format!("an earlier version of {path} of {}", label(&r.asset, &r.container)));
        let t = Target { container: r.container.clone(), runtime: String::new() };
        let h = ngx::history(&self.st.core, &r.asset, &t, &path).await?;
        let v = h.versions.iter().find(|v| v.id == r.id).ok_or_else(|| AppError::NotFound("that version".into()))?;
        Ok(reply(pb::NginxFile { path, content: v.content.clone(), rev: ngx::rev(&v.content), versions: h.versions.len() as u32 }, &me))
    }

    async fn read_log(&self, req: Request<pb::NginxLogRequest>) -> Reply<pb::NginxLog> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        let path = ngx::clean_path(&r.path)?;
        crate::audit::target(format!("log {path} of {}", label(&r.asset, &r.container)));
        let tail = if r.tail == 0 { 200 } else { r.tail.min(2000) };
        let o = self.nginx(&r.asset, &r.container).await?;
        let res = exec(&o.handle, &o.target.log_cmd(&path, tail), b"", 30, 1024 * 1024).await;
        let target = o.target.clone();
        o.close().await;
        let (code, out) = res?;
        let log = match code {
            // Not a regular file: the official images link the logs to stdout / stderr.
            8 => pb::NginxLog { text: String::new(), container_output: true },
            4 => return Err(AppError::NotFound(format!("{path} is not there")).into()),
            5 => return Err(AppError::BadRequest("that is not one of nginx's log files".into()).into()),
            c => {
                ngx::expect_ran(c, &out, &target)?;
                pb::NginxLog { text: out, container_output: false }
            }
        };
        Ok(reply(log, &me))
    }
}
