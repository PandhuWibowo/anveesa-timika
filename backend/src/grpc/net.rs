use std::time::{Duration, Instant};

use tonic::Request;

use super::pb::net_service_server::NetService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::bastion::{self, ssh, Secret};
use crate::error::AppError;
use crate::nettools::{Check, MAX_OUTPUT};

#[tonic::async_trait]
impl NetService for Ctx {
    async fn run(&self, req: Request<pb::NetRequest>) -> Reply<pb::NetResult> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let core = &self.st.core;
        let r = req.get_ref();
        let check = Check::new(r)?;
        let asset = bastion::get_asset(core, &r.asset).await?;
        // Only as an account the caller could open a terminal with.
        let allowed = bastion::allowed_accounts(core, &me, &asset).await?;
        let account = match r.account.as_str() {
            "" => allowed.first().cloned(),
            a => allowed.iter().find(|x| *x == a).cloned(),
        }
        .ok_or_else(|| AppError::Forbidden(format!("you don't have access to {}", asset.name)))?;
        crate::audit::target(format!("{} from {account}@{}", check.describe(), asset.id));

        let secret: Secret = core.get_json(&bastion::cred_path(&asset.id, &account)).await?.ok_or_else(|| AppError::NotFound("account credentials".into()))?;
        let c = ssh::connect(&asset, &account, &secret).await.map_err(AppError::Unavailable)?;
        if c.first_seen && !c.host_key.is_empty() {
            let _ = bastion::pin_host_key(core, &asset.id, &c.host_key).await;
        }
        let started = Instant::now();
        let res = ssh::exec_out(&c.handle, &check.command(), b"", Duration::from_secs(check.secs()), MAX_OUTPUT).await;
        ssh::disconnect(&c.handle).await;
        let (code, out) = res.map_err(AppError::Unavailable)?;

        let mut result = check.parse(code, &out);
        result.took_ms = started.elapsed().as_millis() as u32;
        result.source = asset.name.clone();
        result.account = account;
        Ok(reply(result, &me))
    }
}
