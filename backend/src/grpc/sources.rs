use std::collections::BTreeMap;

use tonic::Request;

use super::convert::ts;
use super::pb::source_service_server::SourceService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::error::AppResult;
use crate::sources::{self as src, Source, State};

fn info(s: &Source, st: &State, has_secret: bool) -> pb::SourceInfo {
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for i in &st.inventory.items {
        *counts.entry(i.kind.as_str()).or_default() += 1;
    }
    pb::SourceInfo {
        id: s.id.clone(),
        name: s.name.clone(),
        kind: s.kind.clone(),
        access: s.access.clone(),
        asset: s.asset.clone(),
        regions: s.regions.clone(),
        scope: s.scope.clone(),
        endpoint: s.endpoint.clone(),
        has_secret,
        read_at: st.at.map(ts),
        error: st.error.clone(),
        counts: counts.into_iter().map(|(kind, count)| pb::SourceCount { kind: kind.into(), count }).collect(),
        notes: st.inventory.notes.clone(),
    }
}

impl Ctx {
    async fn source_info(&self, s: &Source) -> AppResult<pb::SourceInfo> {
        let core = &self.st.core;
        Ok(info(s, &src::state(core, &s.id).await?, src::has_secret(core, &s.id).await?))
    }
}

#[tonic::async_trait]
impl SourceService for Ctx {
    async fn list_sources(&self, req: Request<()>) -> Reply<pb::ListSourcesResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let mut sources = Vec::new();
        for s in src::all(&self.st.core).await? {
            sources.push(self.source_info(&s).await?);
        }
        Ok(reply(pb::ListSourcesResponse { sources }, &me))
    }

    async fn save_source(&self, req: Request<pb::SaveSourceRequest>) -> Reply<pb::SourceInfo> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let i = r.source.unwrap_or_default();
        crate::audit::target(format!("source {} ({})", i.name, i.kind));
        let s = Source { id: i.id, name: i.name, kind: i.kind, access: i.access, asset: i.asset, regions: i.regions, scope: i.scope, endpoint: i.endpoint };
        let saved = src::save(&self.st.core, s, r.secret.into_iter().collect()).await?;
        Ok(reply(self.source_info(&saved).await?, &me))
    }

    async fn delete_source(&self, req: Request<pb::SourceRef>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = &req.get_ref().id;
        crate::audit::target(format!("source {id}"));
        src::delete(&self.st.core, id).await?;
        Ok(reply((), &me))
    }

    async fn refresh_source(&self, req: Request<pb::SourceRef>) -> Reply<pb::SourceInfo> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let s = src::get(core, &req.get_ref().id).await?;
        crate::audit::target(format!("source {} ({})", s.name, s.kind));
        let st = src::refresh(core, &s).await?;
        Ok(reply(info(&s, &st, src::has_secret(core, &s.id).await?), &me))
    }
}
