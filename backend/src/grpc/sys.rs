use serde_json::{json, Value};
use tonic::{Request, Response, Status};

use super::convert::{str_opt, ts, value_struct};
use super::pb::sys_service_server::SysService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::core::{KeyStatus, SealStatus};

pub fn seal_status_pb(s: SealStatus) -> pb::SealStatus {
    pb::SealStatus {
        initialized: s.initialized,
        sealed: s.sealed,
        t: s.t as u32,
        n: s.n as u32,
        progress: s.progress as u32,
        storage: s.storage.to_string(),
        node: s.node,
        joining: s.joining,
        seal_type: s.seal_type,
        auto_unseal: s.auto_unseal,
        auto_unseal_error: s.auto_unseal_error,
        auto_unseal_paused: s.auto_unseal_paused,
    }
}

fn key_status_pb(k: KeyStatus) -> pb::KeyStatus {
    pb::KeyStatus { term: k.term, install_time: ts(k.install_time), terms: k.terms as u32, encryption: k.encryption.to_string() }
}

#[tonic::async_trait]
impl SysService for Ctx {
    async fn health(&self, _: Request<()>) -> Reply<pb::HealthResponse> {
        let core = &self.st.core;
        let status = core.status().await.ok();
        let (standby, leader) = match core.storage.raft() {
            Some(r) => {
                let leader = r.peers().await.into_iter().find(|p| p["leader"] == true).and_then(|p| str_opt(&p["name"]));
                (!r.is_leader().await, leader)
            }
            None => (false, None),
        };
        Ok(Response::new(pb::HealthResponse {
            version: env!("CARGO_PKG_VERSION").into(),
            engine: core.storage.kind().into(),
            initialized: status.as_ref().is_some_and(|s| s.initialized),
            sealed: status.as_ref().is_none_or(|s| s.sealed),
            standby,
            node: core.storage.raft().map(|r| r.cfg.node.name.clone()),
            leader,
        }))
    }

    async fn get_seal_status(&self, _: Request<()>) -> Reply<pb::SealStatus> {
        Ok(Response::new(seal_status_pb(self.st.core.status().await?)))
    }

    async fn init(&self, req: Request<pb::InitRequest>) -> Reply<pb::InitResponse> {
        let r = req.into_inner();
        let shares = u8::try_from(r.secret_shares).map_err(|_| Status::invalid_argument("secret_shares too large"))?;
        let threshold = u8::try_from(r.secret_threshold).map_err(|_| Status::invalid_argument("secret_threshold too large"))?;
        let out = self.st.core.initialize(shares, threshold).await?;
        Ok(Response::new(pb::InitResponse { keys: out.keys, root_token: out.root_token, seal_type: out.seal_type }))
    }

    async fn unseal(&self, req: Request<pb::UnsealRequest>) -> Reply<pb::UnsealResponse> {
        let r = req.into_inner();
        let core = &self.st.core;
        crate::audit::target(if r.reset { "reset progress" } else if r.all { "all instances" } else { "this instance" });
        if r.reset {
            return Ok(Response::new(pb::UnsealResponse { status: Some(seal_status_pb(core.unseal_reset().await?)), instances: vec![] }));
        }
        let mut instances = Vec::new();
        if r.all {
            // Fan out first (unsealed instances treat it as a no-op), then ourselves.
            let others: Vec<Value> = core.instances().await?["instances"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|i| i["self"] != true && i["stale"] != true)
                .collect();
            for i in others {
                let id = str_opt(&i["id"]).unwrap_or_default();
                let addr = str_opt(&i["api_addr"]).unwrap_or_default();
                instances.push(match super::client::unseal(&addr, &r.key).await {
                    Ok(s) => pb::InstanceUnseal { id, sealed: Some(s.sealed), progress: s.progress, error: None },
                    Err(e) => pb::InstanceUnseal { id, sealed: None, progress: 0, error: Some(e.to_string()) },
                });
            }
        }
        // A bad key fails here with the proper status (the others rejected it too).
        let status = seal_status_pb(core.unseal(&r.key).await?);
        Ok(Response::new(pb::UnsealResponse { status: Some(status), instances }))
    }

    async fn instances(&self, _: Request<()>) -> Reply<pb::InstancesResponse> {
        let v = self.st.core.instances().await?;
        let instances = v["instances"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|i| pb::Instance {
                id: str_opt(&i["id"]).unwrap_or_default(),
                api_addr: str_opt(&i["api_addr"]).unwrap_or_default(),
                sealed: i["sealed"].as_bool(),
                stale: i["stale"] == true,
                self_: i["self"] == true,
                leader: i["leader"] == true,
                voter: i["voter"] == true,
                joining: i["joining"] == true,
                version: str_opt(&i["version"]).unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(pb::InstancesResponse {
            storage: str_opt(&v["storage"]).unwrap_or_default(),
            self_: str_opt(&v["self"]).unwrap_or_default(),
            instances,
        }))
    }

    async fn seal(&self, req: Request<pb::SealRequest>) -> Reply<pb::SealResponse> {
        crate::audit::target(if req.get_ref().all { "all instances" } else { "this instance" });
        let (me, _) = self.who(&req, Need::Admin).await?;
        let report = if req.get_ref().all {
            self.st.core.seal_all().await?
        } else {
            self.st.core.seal().await;
            json!({ "scope": "this instance" })
        };
        let status = seal_status_pb(self.st.core.status().await?);
        Ok(reply(pb::SealResponse { status: Some(status), report: Some(value_struct(&report)) }, &me))
    }

    async fn rotate(&self, req: Request<()>) -> Reply<pb::KeyStatus> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        Ok(reply(key_status_pb(self.st.core.rotate().await?), &me))
    }

    async fn get_key_status(&self, req: Request<()>) -> Reply<pb::KeyStatus> {
        let (me, _) = self.who(&req, Need::Read).await?;
        Ok(reply(key_status_pb(self.st.core.key_status().await?), &me))
    }

    async fn storage(&self, req: Request<()>) -> Reply<prost_types::Struct> {
        let (me, _) = self.who(&req, Need::Read).await?;
        Ok(reply(value_struct(&self.st.core.storage.describe().await?), &me))
    }

    async fn audit(&self, req: Request<()>) -> Reply<prost_types::Struct> {
        let (me, _) = self.who(&req, Need::Read).await?;
        let v = match &self.st.audit {
            Some(a) => a.describe().await,
            None => crate::audit::disabled(),
        };
        Ok(reply(value_struct(&v), &me))
    }

    async fn audit_hash(&self, req: Request<pb::AuditHashRequest>) -> Reply<pb::AuditHashResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let hash = self.st.core.audit_hmac(req.get_ref().input.as_bytes()).await.ok_or_else(|| Status::failed_precondition("vault is sealed"))?;
        Ok(reply(pb::AuditHashResponse { hash }, &me))
    }
}
