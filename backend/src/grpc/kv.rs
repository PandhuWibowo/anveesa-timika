use tonic::{Request, Response};

use super::convert::{from_struct, to_struct, ts, ts_opt};
use super::pb::kv_service_server::KvService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::kv::{self, clean_path};

fn version_pb(v: kv::VersionMeta) -> pb::VersionMeta {
    pb::VersionMeta { created_time: ts(v.created_time), deletion_time: ts_opt(v.deletion_time) }
}

fn meta_pb(m: kv::Metadata) -> pb::Metadata {
    pb::Metadata {
        current_version: m.current_version,
        oldest_version: m.oldest_version,
        max_versions: m.max_versions,
        created_time: ts(m.created_time),
        updated_time: ts(m.updated_time),
        versions: m.versions.into_iter().map(|(k, v)| (k, version_pb(v))).collect(),
    }
}

#[tonic::async_trait]
impl KvService for Ctx {
    async fn list(&self, req: Request<pb::ListRequest>) -> Reply<pb::ListResponse> {
        crate::audit::target(format!("{}/", req.get_ref().folder.trim_matches('/')));
        let (me, _) = self.who(&req, Need::Read).await?;
        let l = kv::list(&self.st.core, &req.get_ref().folder).await?;
        Ok(reply(pb::ListResponse { keys: l.keys }, &me))
    }

    async fn read(&self, req: Request<pb::ReadRequest>) -> Reply<pb::Secret> {
        crate::audit::target(match req.get_ref().version { Some(v) => format!("{} (v{v})", req.get_ref().path), None => req.get_ref().path.clone() });
        let (me, _) = self.who(&req, Need::Read).await?;
        let r = req.get_ref();
        let s = kv::read(&self.st.core, &clean_path(&r.path)?, r.version).await?;
        Ok(reply(pb::Secret { data: Some(to_struct(&s.data)), version: s.version, metadata: Some(version_pb(s.metadata)) }, &me))
    }

    async fn write(&self, req: Request<pb::WriteRequest>) -> Reply<pb::Metadata> {
        crate::audit::target(req.get_ref().path.clone());
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let path = clean_path(&r.path)?;
        let data = from_struct(r.data.unwrap_or_default());
        let m = kv::write(&self.st.core, &path, data, r.cas, self.st.cfg.kv_max_versions).await?;
        Ok(reply(meta_pb(m), &me))
    }

    async fn delete(&self, req: Request<pb::DeleteRequest>) -> Reply<pb::Metadata> {
        crate::audit::target(if req.get_ref().versions.is_empty() { format!("{} (latest)", req.get_ref().path) } else { format!("{} (v{})", req.get_ref().path, req.get_ref().versions.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", v")) });
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let versions = (!r.versions.is_empty()).then_some(r.versions);
        let m = kv::soft_delete(&self.st.core, &clean_path(&r.path)?, versions).await?;
        Ok(reply(meta_pb(m), &me))
    }

    async fn get_metadata(&self, req: Request<pb::PathRequest>) -> Reply<pb::Metadata> {
        crate::audit::target(req.get_ref().path.clone());
        let (me, _) = self.who(&req, Need::Read).await?;
        let m = kv::metadata(&self.st.core, &clean_path(&req.get_ref().path)?).await?;
        Ok(reply(meta_pb(m), &me))
    }

    async fn destroy(&self, req: Request<pb::PathRequest>) -> Reply<()> {
        crate::audit::target(req.get_ref().path.clone());
        let (me, _) = self.who(&req, Need::Admin).await?;
        kv::destroy(&self.st.core, &clean_path(&req.get_ref().path)?).await?;
        Ok(reply((), &me))
    }
}

// Silence the unused-import lint when Response isn't needed directly.
#[allow(dead_code)]
type _R = Response<()>;
