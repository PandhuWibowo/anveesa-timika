use chrono::Utc;
use serde_json::Value;
use tonic::Request;

use super::pb::container_service_server::ContainerService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::bastion::ssh;
use crate::containers as dk;
use crate::error::{AppError, AppResult};
use crate::monitor::{self as mon, SystemCfg};

impl Ctx {
    /// Run one command on a monitored server as its monitoring account.
    pub async fn on_host(&self, asset: &str, cmd: &str, max: usize, secs: u64) -> AppResult<(u32, String)> {
        let core = &self.st.core;
        let cfg: SystemCfg = core
            .get_json(&mon::system_path(asset))
            .await?
            .ok_or_else(|| AppError::NotFound("that server is not monitored — add it under Monitoring → Systems first".into()))?;
        let handle = mon::engine::connect(core, &cfg).await.map_err(AppError::Unavailable)?;
        let res = ssh::exec_out(&handle, cmd, b"", std::time::Duration::from_secs(secs), max).await;
        ssh::disconnect(&handle).await;
        res.map_err(AppError::Unavailable)
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn detail(v: &Value) -> pb::ContainerDetail {
    let cfg = &v["Config"];
    // Podman gives the entrypoint as one string, Docker as a list.
    let mut cmd: Vec<String> = match &cfg["Entrypoint"] {
        Value::String(e) if !e.is_empty() => vec![e.clone()],
        e => e.as_array().into_iter().flatten().map(s).collect(),
    };
    cmd.extend(cfg["Cmd"].as_array().into_iter().flatten().map(s));
    let mut ports = Vec::new();
    for (port, binds) in v["NetworkSettings"]["Ports"].as_object().into_iter().flatten() {
        match binds.as_array() {
            Some(b) if !b.is_empty() => {
                for x in b {
                    ports.push(format!("{}:{} → {port}", s(&x["HostIp"]), s(&x["HostPort"])));
                }
            }
            _ => ports.push(format!("{port} (not published)")),
        }
    }
    ports.sort();
    ports.dedup();
    let labels = &cfg["Labels"];
    pb::ContainerDetail {
        id: s(&v["Id"]).chars().take(12).collect(),
        name: s(&v["Name"]).trim_start_matches('/').to_string(),
        image: s(&cfg["Image"]),
        command: cmd.join(" "),
        created: s(&v["Created"]),
        started: s(&v["State"]["StartedAt"]),
        state: s(&v["State"]["Status"]),
        health: s(&v["State"]["Health"]["Status"]),
        exit_code: v["State"]["ExitCode"].as_i64().unwrap_or(0) as i32,
        restart_policy: s(&v["HostConfig"]["RestartPolicy"]["Name"]),
        working_dir: s(&cfg["WorkingDir"]),
        user: s(&cfg["User"]),
        env: cfg["Env"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str())
            .map(|e| {
                let (name, value) = e.split_once('=').unwrap_or((e, ""));
                pb::EnvVar { name: name.into(), value: value.into() }
            })
            .collect(),
        mounts: v["Mounts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| pb::Mount {
                kind: s(&m["Type"]),
                source: if m["Type"] == "volume" { s(&m["Name"]) } else { s(&m["Source"]) },
                destination: s(&m["Destination"]),
                read_only: m["RW"].as_bool() == Some(false),
            })
            .collect(),
        networks: v["NetworkSettings"]["Networks"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, n)| pb::ContainerNetwork { name: name.clone(), ip: s(&n["IPAddress"]) })
            .collect(),
        ports,
        compose_project: s(&labels["com.docker.compose.project"]),
        compose_service: s(&labels["com.docker.compose.service"]),
    }
}

fn result(code: u32, out: String) -> pb::ActionResult {
    pb::ActionResult { ok: code == 0, output: out.trim().chars().take(4000).collect() }
}

#[tonic::async_trait]
impl ContainerService for Ctx {
    async fn inspect_container(&self, req: Request<pb::ContainerRef>) -> Reply<pb::ContainerDetail> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        dk::check_name(&r.name)?;
        crate::audit::target(format!("container {} on {} (details)", r.name, r.asset));
        let (code, out) = self.on_host(&r.asset, &dk::inspect_cmd(&mon::runtime_of(&self.st.core, &r.asset, &r.name).await, &r.name), 512 * 1024, 30).await?;
        dk::expect_ok(code, &out)?;
        let v: Value = serde_json::from_str(&out).map_err(|_| AppError::Unavailable("docker inspect returned something unexpected".into()))?;
        let first = v.get(0).ok_or_else(|| AppError::NotFound(format!("container `{}`", r.name)))?;
        Ok(reply(detail(first), &me))
    }

    async fn list_files(&self, req: Request<pb::ContainerPath>) -> Reply<pb::ContainerFiles> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        dk::check_name(&r.name)?;
        let path = dk::clean_path(&r.path)?;
        crate::audit::target(format!("container {} on {}:{path}", r.name, r.asset));
        let (code, out) = self.on_host(&r.asset, &dk::list_cmd(&mon::runtime_of(&self.st.core, &r.asset, &r.name).await, &r.name, &path), 2 * 1024 * 1024, 30).await?;
        let entries = dk::parse_list(code, &out)?
            .into_iter()
            .map(|e| pb::ContainerFile { name: e.name, kind: e.kind, size: e.size, modified: e.modified, mode: e.mode })
            .collect();
        Ok(reply(pb::ContainerFiles { path, entries }, &me))
    }

    async fn make_dir(&self, req: Request<pb::ContainerPath>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        dk::check_name(&r.name)?;
        let path = dk::clean_path(&r.path)?;
        crate::audit::target(format!("container {} on {}:{path} (new folder)", r.name, r.asset));
        let (code, out) = self.on_host(&r.asset, &dk::mkdir_cmd(&mon::runtime_of(&self.st.core, &r.asset, &r.name).await, &r.name, &path), 16 * 1024, 30).await?;
        dk::expect_ok(code, &out)?;
        Ok(reply((), &me))
    }

    async fn delete_files(&self, req: Request<pb::ContainerPaths>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        dk::check_name(&r.name)?;
        let paths = r.paths.iter().map(|p| dk::clean_path(p)).collect::<AppResult<Vec<String>>>()?;
        if paths.is_empty() || paths.len() > 200 || paths.iter().any(|p| p == "/") {
            return Err(AppError::BadRequest("choose what to delete (not /)".into()).into());
        }
        crate::audit::target(format!("container {} on {}:{} (delete)", r.name, r.asset, paths.join(", ")));
        let (code, out) = self.on_host(&r.asset, &dk::delete_cmd(&mon::runtime_of(&self.st.core, &r.asset, &r.name).await, &r.name, &paths), 16 * 1024, 60).await?;
        dk::expect_ok(code, &out)?;
        Ok(reply((), &me))
    }

    async fn download_link(&self, req: Request<pb::ContainerPath>) -> Reply<pb::ContainerLink> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        dk::check_name(&r.name)?;
        let path = dk::clean_path(&r.path)?;
        crate::audit::target(format!("container {} on {}:{path} (download link)", r.name, r.asset));
        // A folder (or an image without a shell) goes out as a tar.
        let rt = mon::runtime_of(&self.st.core, &r.asset, &r.name).await;
        let (code, out) = self.on_host(&r.asset, &dk::kind_cmd(&rt, &r.name, &path), 4096, 30).await?;
        let kind = match (code, out.trim()) {
            (0, "file") => "file",
            (0, "none") => return Err(AppError::NotFound(format!("`{path}` in the container")).into()),
            _ => "dir",
        };
        let key = self.st.core.derive_key(b"timika container ticket v1").await.ok_or(AppError::Sealed)?;
        let t = dk::Ticket {
            asset: r.asset.clone(),
            container: r.name.clone(),
            path: path.clone(),
            runtime: rt,
            kind: kind.into(),
            who: me.display_name.clone(),
            username: me.username.clone(),
            policies: me.policies.clone(),
            exp: Utc::now().timestamp() + 60,
        };
        let base = path.rsplit('/').find(|x| !x.is_empty()).unwrap_or(&r.name).to_string();
        let name = if kind == "dir" { format!("{base}.tar") } else { base };
        Ok(reply(pb::ContainerLink { url: format!("/v1/containers/download?t={}", dk::sign(&key, &t)), name }, &me))
    }

    async fn list_images(&self, req: Request<pb::RuntimeHost>) -> Reply<pb::ListImagesResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let asset = &req.get_ref().asset;
        let mut images = Vec::new();
        for rt in mon::runtimes(&self.st.core, asset).await {
            let (code, out) = self.on_host(asset, &dk::images_cmd(&rt), 1024 * 1024, 30).await?;
            dk::expect_ok(code, &out)?;
            images.extend(dk::parse_images(&out).into_iter().map(|i| pb::Image {
                id: i.id,
                repository: i.repository,
                tag: i.tag,
                size: i.size,
                created: i.created,
                used_by: i.used_by,
                dangling: i.dangling,
                runtime: rt.clone(),
            }));
        }
        Ok(reply(pb::ListImagesResponse { images }, &me))
    }

    async fn remove_image(&self, req: Request<pb::ImageRef>) -> Reply<pb::ActionResult> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        dk::check_ref(&r.image)?;
        crate::audit::target(format!("image {} on {} (remove)", r.image, r.asset));
        let (code, out) = self.on_host(&r.asset, &dk::rmi_cmd(&r.runtime, &r.image), 64 * 1024, 120).await?;
        Ok(reply(result(code, out), &me))
    }

    async fn prune_images(&self, req: Request<pb::RuntimeHost>) -> Reply<pb::ActionResult> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let asset = &req.get_ref().asset;
        crate::audit::target(format!("images on {asset} (prune dangling)"));
        let (mut ok, mut text) = (true, Vec::new());
        for rt in mon::runtimes(&self.st.core, asset).await {
            let (code, out) = self.on_host(asset, &dk::image_prune_cmd(&rt), 64 * 1024, 300).await?;
            ok &= code == 0;
            text.push(out.trim().to_string());
        }
        Ok(reply(result(if ok { 0 } else { 1 }, text.join("\n")), &me))
    }

    async fn list_volumes(&self, req: Request<pb::RuntimeHost>) -> Reply<pb::ListVolumesResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let asset = &req.get_ref().asset;
        let mut volumes = Vec::new();
        for rt in mon::runtimes(&self.st.core, asset).await {
            let (code, out) = self.on_host(asset, &dk::volumes_cmd(&rt), 1024 * 1024, 60).await?;
            dk::expect_ok(code, &out)?;
            volumes.extend(dk::parse_volumes(&out).into_iter().map(|v| pb::Volume { name: v.name, driver: v.driver, mountpoint: v.mountpoint, size: v.size, used_by: v.used_by, runtime: rt.clone() }));
        }
        Ok(reply(pb::ListVolumesResponse { volumes }, &me))
    }

    async fn remove_volume(&self, req: Request<pb::VolumeRef>) -> Reply<pb::ActionResult> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        dk::check_ref(&r.name)?;
        crate::audit::target(format!("volume {} on {} (remove)", r.name, r.asset));
        let (code, out) = self.on_host(&r.asset, &dk::volume_rm_cmd(&r.runtime, &r.name), 64 * 1024, 120).await?;
        Ok(reply(result(code, out), &me))
    }

    async fn prune_volumes(&self, req: Request<pb::RuntimeHost>) -> Reply<pb::ActionResult> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let asset = &req.get_ref().asset;
        crate::audit::target(format!("volumes on {asset} (prune unused)"));
        let (mut ok, mut text) = (true, Vec::new());
        for rt in mon::runtimes(&self.st.core, asset).await {
            let (code, out) = self.on_host(asset, &dk::volume_prune_cmd(&rt), 64 * 1024, 300).await?;
            ok &= code == 0;
            text.push(out.trim().to_string());
        }
        Ok(reply(result(if ok { 0 } else { 1 }, text.join("\n")), &me))
    }
}
