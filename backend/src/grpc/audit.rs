use tonic::{Request, Status};

use super::convert::value_struct;
use super::pb::audit_service_server::AuditService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::audit::{self, Event, Filter};

fn event_pb(e: Event) -> pb::AuditEvent {
    pb::AuditEvent {
        seq: e.seq,
        time: e.time,
        id: e.id,
        kind: e.kind.to_string(),
        action: e.action,
        user: e.user,
        policies: e.policies,
        remote_addr: e.remote_addr,
        target: e.target,
        ok: e.ok,
        code: e.code,
        error: e.error,
        duration_ms: e.duration_ms,
        details: e.details.is_object().then(|| value_struct(&e.details)),
        instance: e.instance,
    }
}

#[tonic::async_trait]
impl AuditService for Ctx {
    async fn list_events(&self, req: Request<pb::ListEventsRequest>) -> Reply<pb::ListEventsResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let Some(aud) = &self.st.audit else {
            return Ok(reply(pb::ListEventsResponse { events: vec![], next_before_seq: None, instance: String::new(), stored: false }, &me));
        };
        let instance = aud.instance().to_string();
        let Some(files) = aud.files() else {
            return Ok(reply(pb::ListEventsResponse { events: vec![], next_before_seq: None, instance, stored: false }, &me));
        };
        let limit = match r.limit { 0 => 100, n => n.min(500) } as usize;
        let filter = Filter { user: r.user, query: r.query, outcome: r.outcome, category: r.category, before_seq: r.before_seq, include_routine: r.include_routine, server: r.server };
        // Reading files is blocking work; keep it off the async workers.
        let (events, next) = tokio::task::spawn_blocking(move || audit::read_events(&files, &filter, limit))
            .await
            .map_err(|_| Status::internal("internal error"))?;
        Ok(reply(
            pb::ListEventsResponse { events: events.into_iter().map(event_pb).collect(), next_before_seq: next, instance, stored: true },
            &me,
        ))
    }

    async fn verify(&self, req: Request<()>) -> Reply<pb::VerifyResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let files = self
            .st
            .audit
            .as_ref()
            .and_then(|a| a.files())
            .ok_or_else(|| Status::failed_precondition("this instance keeps no local audit file (AUDIT_FILE)"))?;
        let v = tokio::task::spawn_blocking(move || audit::verify(&files))
            .await
            .map_err(|_| Status::internal("internal error"))?
            .map_err(|e| Status::internal(format!("could not read the audit files: {e}")))?;
        Ok(reply(
            pb::VerifyResponse { ok: v.problems.is_empty(), entries: v.entries, chain_starts: v.chain_starts, problems: v.problems },
            &me,
        ))
    }
}
