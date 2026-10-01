use std::pin::Pin;

use chrono::Utc;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use super::convert::{ts, ts_opt};
use super::pb::automation_service_server::AutomationService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::automation::schedule::{self, Schedule};
use crate::automation::{self as auto, git, notify, runner, Notifier, NotifyTarget, PendingKey, Repo, RepoSecrets, Run, RunOptions, Start, Variable};
use crate::bastion;
use crate::error::{AppError, AppResult};
use crate::token::TokenEntry;

/// Deploy keys not used by a repository within this time are discarded.
const DEPLOY_KEY_TTL_SECS: i64 = 3600;

fn js<T: serde::Serialize>(v: &T) -> AppResult<Vec<u8>> {
    Ok(serde_json::to_vec(v)?)
}

fn who_name(me: &TokenEntry) -> String {
    me.username.clone().unwrap_or_else(|| me.display_name.clone())
}

fn commit_pb(c: &auto::Commit) -> pb::Commit {
    pb::Commit { sha: c.sha.clone(), message: c.message.clone(), author: c.author.clone(), time: c.time.clone() }
}

pub fn opts_pb(o: &RunOptions) -> pb::RunOptions {
    pb::RunOptions {
        limit: o.limit.clone(),
        tags: o.tags.clone(),
        skip_tags: o.skip_tags.clone(),
        extra_vars: o.extra_vars.clone(),
        verbose: o.verbose as u32,
        servers: o.servers.clone(),
        destroy: o.destroy,
        targets: o.targets.clone(),
        replace: o.replace.clone(),
        workspace: o.workspace.clone(),
        refresh: o.refresh,
    }
}

pub fn opts_from(o: Option<pb::RunOptions>) -> RunOptions {
    let o = o.unwrap_or_default();
    RunOptions {
        limit: o.limit,
        tags: o.tags,
        skip_tags: o.skip_tags,
        extra_vars: o.extra_vars,
        verbose: o.verbose.min(9) as u8,
        servers: o.servers,
        destroy: o.destroy,
        targets: o.targets,
        replace: o.replace,
        workspace: o.workspace,
        refresh: o.refresh,
    }
}

fn schedule_pb(s: &Schedule, project_name: String) -> pb::Schedule {
    pb::Schedule {
        id: s.id.clone(),
        repo: s.repo.clone(),
        project: s.project.clone(),
        project_name,
        action: s.action.clone(),
        name: s.name.clone(),
        cron: s.cron.clone(),
        timezone: s.timezone.clone(),
        options: Some(opts_pb(&s.options)),
        enabled: s.enabled,
        next_run: ts_opt(s.next()),
        last_fired: ts_opt(s.last_fired),
        last_run: s.last_run.clone(),
        last_result: s.last_result.clone(),
        created_by: s.created_by.clone(),
    }
}

/// Admin-only secrets shown on the Setup tab.
struct Shown {
    webhook: String,
    trigger: String,
}

fn repo_pb(r: &Repo, runner_name: String, secrets: Option<Shown>) -> pb::Repo {
    let (webhook_secret, trigger_token) = match secrets {
        Some(s) => (Some(s.webhook), Some(s.trigger).filter(|t| !t.is_empty())),
        None => (None, None),
    };
    pb::Repo {
        id: r.id.clone(),
        name: r.name.clone(),
        url: r.url.clone(),
        branch: r.branch.clone(),
        auth: r.auth.clone(),
        deploy_public_key: r.deploy_public_key.clone(),
        runner_asset: r.runner_asset.clone(),
        runner_account: r.runner_account.clone(),
        runner_name,
        auto_plan: r.auto_plan,
        tf_bin: r.tf_bin.clone(),
        variables: r.variables.iter().map(|v| pb::Variable { name: v.name.clone(), value: if v.secret { String::new() } else { v.value.clone().unwrap_or_default() }, secret: v.secret }).collect(),
        head: r.head.as_ref().map(commit_pb),
        synced_at: ts_opt(r.synced_at),
        sync_error: r.sync_error.clone(),
        projects: r
            .projects
            .iter()
            .map(|p| pb::Project { id: p.id.clone(), kind: p.kind.clone(), dir: p.dir.clone(), file: p.file.clone(), stack: p.stack.clone(), name: p.name.clone() })
            .collect(),
        webhook_path: format!("/v1/automation/hooks/{}", r.id),
        webhook_secret,
        last_hook: r.last_hook.as_ref().map(|h| pb::HookDelivery { time: ts(h.time), sha: h.sha.clone(), by: h.by.clone(), result: h.result.clone() }),
        created_at: ts(r.created_at),
        created_by: r.created_by.clone(),
        require_approval: r.require_approval,
        notifiers: r.notifiers.iter().map(|n| pb::Notifier { id: n.id.clone(), kind: n.kind.clone(), on: n.on.clone(), label: n.label.clone() }).collect(),
        trigger_path: format!("/v1/automation/trigger/{}", r.id),
        trigger_token,
    }
}

pub fn run_pb(r: &Run) -> pb::Run {
    pb::Run {
        id: r.id.clone(),
        repo: r.repo.clone(),
        repo_name: r.repo_name.clone(),
        project: r.project.clone(),
        project_name: r.project_name.clone(),
        kind: r.kind.clone(),
        action: r.action.clone(),
        status: r.shown_status().to_string(),
        user: r.user.clone(),
        trigger: r.trigger.clone(),
        sha: r.sha.clone(),
        commit_message: r.commit_message.clone(),
        runner_name: r.runner_name.clone(),
        created_at: ts(r.created_at),
        started_at: ts_opt(r.started_at),
        finished_at: ts_opt(r.finished_at),
        exit_code: r.exit_code,
        summary: r.summary.clone(),
        changes: r.changes,
        error: r.error.clone(),
        plan_run: r.plan_run.clone(),
        applied_by: r.applied_by.clone(),
        log_bytes: r.log_bytes,
        truncated: r.truncated,
        options: Some(opts_pb(&r.options)),
        options_text: r.options.describe(),
        approved_by: r.approved_by.clone(),
    }
}

/// `owner/repo` from a Git URL (the default name).
fn name_from_url(url: &str) -> String {
    let path = url.trim_end_matches('/').trim_end_matches(".git");
    let parts: Vec<&str> = path.split(['/', ':']).filter(|s| !s.is_empty()).collect();
    match parts.as_slice() {
        [.., owner, repo] if !owner.contains('@') && !owner.contains('.') => format!("{owner}/{repo}"),
        [.., repo] => repo.to_string(),
        [] => "repository".into(),
    }
}

impl Ctx {
    /// The repository and the commit to look at (head unless `sha` is given), cloned here.
    async fn repo_at(&self, id: &str, sha: &str) -> AppResult<(Repo, String)> {
        let repo = auto::get_repo(&self.st.core, id).await?;
        let sha = if sha.is_empty() {
            repo.head.as_ref().map(|h| h.sha.clone()).ok_or_else(|| AppError::BadRequest("the repository hasn't been pulled yet".into()))?
        } else {
            sha.to_string()
        };
        let s = auto::get_secrets(&self.st.core, id).await?;
        let auth = git::Auth { token: s.token, private_key: s.private_key };
        git::ensure_commit(&repo.id, &repo.url, &repo.branch, &auth, &sha).await.map_err(AppError::BadRequest)?;
        Ok((repo, sha))
    }

    async fn runner_name(&self, r: &Repo) -> String {
        match bastion::get_asset(&self.st.core, &r.runner_asset).await {
            Ok(a) => format!("{}@{}", r.runner_account, a.name),
            Err(_) => format!("{}@(deleted server)", r.runner_account),
        }
    }

    async fn repo_reply(&self, r: &Repo, me: &TokenEntry) -> AppResult<pb::Repo> {
        let shown = if me.is_admin() {
            let s = auto::get_secrets(&self.st.core, &r.id).await?;
            Some(Shown { webhook: s.webhook_secret, trigger: s.trigger_token })
        } else {
            None
        };
        Ok(repo_pb(r, self.runner_name(r).await, shown))
    }

    /// Validate the runner: the server exists and has that account.
    async fn check_runner_choice(&self, asset: &str, account: &str) -> AppResult<()> {
        let a = bastion::get_asset(&self.st.core, asset).await.map_err(|_| AppError::BadRequest("choose a runner server".into()))?;
        if !a.accounts.iter().any(|x| x.username == account) {
            return Err(AppError::BadRequest(format!("{} has no account `{account}`", a.name)));
        }
        Ok(())
    }

    /// Apply the input's variables (keeping stored secret values left empty).
    fn variables(input: &[pb::VariableInput], old: &RepoSecrets) -> AppResult<(Vec<Variable>, std::collections::HashMap<String, String>)> {
        let mut vars = Vec::new();
        let mut values = std::collections::HashMap::new();
        for v in input {
            let name = v.name.trim().to_string();
            if name.is_empty() && v.value.is_empty() {
                continue;
            }
            if !auto::valid_var_name(&name) {
                return Err(AppError::BadRequest(format!("`{name}` is not a valid variable name (letters, digits, _)")));
            }
            if vars.iter().any(|x: &Variable| x.name == name) {
                return Err(AppError::BadRequest(format!("variable `{name}` is listed twice")));
            }
            if v.secret {
                let value = if v.value.is_empty() { old.vars.get(&name).cloned() } else { Some(v.value.clone()) };
                let Some(value) = value else { return Err(AppError::BadRequest(format!("secret `{name}` needs a value"))) };
                values.insert(name.clone(), value);
                vars.push(Variable { name, value: None, secret: true });
            } else {
                vars.push(Variable { name, value: Some(v.value.clone()), secret: false });
            }
        }
        Ok((vars, values))
    }

    async fn take_deploy_key(&self, id: &str) -> AppResult<PendingKey> {
        let k: PendingKey = self
            .st
            .core
            .get_json(&auto::deploy_key_path(id))
            .await?
            .ok_or_else(|| AppError::BadRequest("that deploy key expired — generate a new one".into()))?;
        if (Utc::now() - k.created_at).num_seconds() > DEPLOY_KEY_TTL_SECS {
            return Err(AppError::BadRequest("that deploy key expired — generate a new one".into()));
        }
        Ok(k)
    }
}

type EventStream = Pin<Box<dyn Stream<Item = Result<pb::RunEvent, Status>> + Send>>;

#[tonic::async_trait]
impl AutomationService for Ctx {
    async fn list_repos(&self, req: Request<()>) -> Reply<pb::ListReposResponse> {
        let (me, _) = self.who(&req, Need::Read).await?;
        let mut repos = Vec::new();
        for r in auto::list_repos(&self.st.core).await? {
            repos.push(self.repo_reply(&r, &me).await?);
        }
        Ok(reply(pb::ListReposResponse { repos }, &me))
    }

    async fn get_repo(&self, req: Request<pb::RepoRef>) -> Reply<pb::RepoDetail> {
        let (me, _) = self.who(&req, Need::Read).await?;
        let r = auto::get_repo(&self.st.core, &req.get_ref().id).await?;
        let commits = git::log(&r.id, &r.branch, 20).await.iter().map(commit_pb).collect();
        Ok(reply(pb::RepoDetail { repo: Some(self.repo_reply(&r, &me).await?), commits }, &me))
    }

    async fn new_deploy_key(&self, req: Request<()>) -> Reply<pb::DeployKey> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = bastion::random_id(8);
        let (private_key, public_key) = git::new_key_pair(&format!("timika-{id}")).map_err(AppError::Internal)?;
        let k = PendingKey { private_key, public_key: public_key.clone(), created_at: Utc::now() };
        // Drop expired ones while we're here.
        let mut deletes = Vec::new();
        for key in self.st.core.list("automation/deploy-keys/").await? {
            if let Some(old) = self.st.core.get_json::<PendingKey>(&key).await? {
                if (Utc::now() - old.created_at).num_seconds() > DEPLOY_KEY_TTL_SECS {
                    deletes.push(key);
                }
            }
        }
        self.st.core.commit(vec![(auto::deploy_key_path(&id), js(&k)?)], deletes, vec![]).await?;
        Ok(reply(pb::DeployKey { id, public_key }, &me))
    }

    async fn create_repo(&self, req: Request<pb::RepoInput>) -> Reply<pb::Repo> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let r = req.into_inner();
        let url = r.url.trim().to_string();
        git::check_url(&url).map_err(AppError::BadRequest)?;
        let name = if r.name.trim().is_empty() { name_from_url(&url) } else { r.name.trim().chars().take(100).collect() };
        crate::audit::target(format!("repository {name} ({url})"));
        self.check_runner_choice(&r.runner_asset, &r.runner_account).await?;
        let (variables, values) = Self::variables(&r.variables, &RepoSecrets::default())?;
        let mut secrets = RepoSecrets { webhook_secret: bastion::random_id(24), trigger_token: format!("tmkci.{}", bastion::random_id(24)), vars: values, ..Default::default() };
        let mut public = None;
        match r.auth.as_str() {
            "none" | "" => {}
            "token" => {
                if r.token.trim().is_empty() {
                    return Err(AppError::BadRequest("enter the access token".into()).into());
                }
                secrets.token = Some(r.token.trim().to_string());
            }
            "deploy_key" => {
                let k = self.take_deploy_key(&r.deploy_key_id).await?;
                secrets.private_key = Some(k.private_key);
                public = Some(k.public_key);
            }
            other => return Err(AppError::BadRequest(format!("unknown access `{other}`")).into()),
        }
        let id = bastion::random_id(6);
        let auth = git::Auth { token: secrets.token.clone(), private_key: secrets.private_key.clone() };
        let branch = match r.branch.trim() {
            "" => git::default_branch(&id, &url, &auth).await.map_err(|e| AppError::BadRequest(format!("can't read the repository: {e}")))?,
            b => b.to_string(),
        };
        git::check_branch(&branch).map_err(AppError::BadRequest)?;
        // Clone now, so wrong access fails here and not at the first run.
        let synced = match git::sync(&id, &url, &branch, &auth).await {
            Ok(s) => s,
            Err(e) => {
                git::remove(&id);
                return Err(AppError::BadRequest(format!("can't read the repository: {e}")).into());
            }
        };
        let repo = Repo {
            id: id.clone(),
            name,
            url,
            branch,
            auth: if r.auth.is_empty() { "none".into() } else { r.auth.clone() },
            deploy_public_key: public,
            runner_asset: r.runner_asset,
            runner_account: r.runner_account,
            auto_plan: r.auto_plan,
            tf_bin: if matches!(r.tf_bin.as_str(), "terraform" | "tofu") { r.tf_bin } else { "auto".into() },
            variables,
            head: Some(synced.head),
            synced_at: Some(Utc::now()),
            sync_error: None,
            projects: synced.projects,
            last_hook: None,
            require_approval: false,
            notifiers: vec![],
            created_at: Utc::now(),
            created_by: who_name(&me),
        };
        let deletes = if r.auth == "deploy_key" { vec![auto::deploy_key_path(&r.deploy_key_id)] } else { vec![] };
        core.commit(
            vec![(auto::repo_path(&id), js(&repo)?), (auto::secrets_path(&id), js(&secrets)?)],
            deletes,
            vec![],
        )
        .await?;
        Ok(reply(self.repo_reply(&repo, &me).await?, &me))
    }

    async fn update_repo(&self, req: Request<pb::UpdateRepoRequest>) -> Reply<pb::Repo> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let u = req.into_inner();
        let r = u.repo.ok_or_else(|| AppError::BadRequest("missing repository".into()))?;
        let old = auto::get_repo(core, &u.id).await?;
        let old_secrets = auto::get_secrets(core, &u.id).await?;
        crate::audit::target(format!("repository {}", old.name));
        let url = if r.url.trim().is_empty() { old.url.clone() } else { r.url.trim().to_string() };
        git::check_url(&url).map_err(AppError::BadRequest)?;
        self.check_runner_choice(&r.runner_asset, &r.runner_account).await?;
        let (variables, values) = Self::variables(&r.variables, &old_secrets)?;
        let mut secrets = RepoSecrets {
            vars: values,
            webhook_secret: old_secrets.webhook_secret.clone(),
            trigger_token: old_secrets.trigger_token.clone(),
            notify: old_secrets.notify.clone(),
            ..Default::default()
        };
        if u.new_webhook_secret {
            secrets.webhook_secret = bastion::random_id(24);
        }
        let auth_kind = if r.auth.is_empty() { old.auth.clone() } else { r.auth.clone() };
        let mut public = None;
        let mut used_key = None;
        match auth_kind.as_str() {
            "none" => {}
            "token" => {
                secrets.token = if r.token.trim().is_empty() { old_secrets.token.clone() } else { Some(r.token.trim().to_string()) };
                if secrets.token.is_none() {
                    return Err(AppError::BadRequest("enter the access token".into()).into());
                }
            }
            "deploy_key" => {
                if r.deploy_key_id.is_empty() {
                    secrets.private_key = old_secrets.private_key.clone();
                    public = old.deploy_public_key.clone();
                    if secrets.private_key.is_none() {
                        return Err(AppError::BadRequest("generate a deploy key".into()).into());
                    }
                } else {
                    let k = self.take_deploy_key(&r.deploy_key_id).await?;
                    secrets.private_key = Some(k.private_key);
                    public = Some(k.public_key);
                    used_key = Some(auto::deploy_key_path(&r.deploy_key_id));
                }
            }
            other => return Err(AppError::BadRequest(format!("unknown access `{other}`")).into()),
        }
        let auth = git::Auth { token: secrets.token.clone(), private_key: secrets.private_key.clone() };
        let branch = match r.branch.trim() {
            "" => old.branch.clone(),
            b => b.to_string(),
        };
        git::check_branch(&branch).map_err(AppError::BadRequest)?;
        let access_changed = url != old.url || branch != old.branch || auth_kind != old.auth || !r.token.is_empty() || used_key.is_some();
        let synced = if access_changed {
            if url != old.url {
                git::remove(&u.id);
            }
            Some(git::sync(&u.id, &url, &branch, &auth).await.map_err(|e| AppError::BadRequest(format!("can't read the repository: {e}")))?)
        } else {
            None
        };
        let mut repo = old.clone();
        repo.name = if r.name.trim().is_empty() { old.name.clone() } else { r.name.trim().chars().take(100).collect() };
        repo.url = url;
        repo.branch = branch;
        repo.auth = auth_kind;
        repo.deploy_public_key = public;
        repo.runner_asset = r.runner_asset;
        repo.runner_account = r.runner_account;
        repo.auto_plan = r.auto_plan;
        repo.tf_bin = if matches!(r.tf_bin.as_str(), "terraform" | "tofu") { r.tf_bin } else { "auto".into() };
        repo.variables = variables;
        if let Some(s) = synced {
            repo.head = Some(s.head);
            repo.projects = s.projects;
            repo.synced_at = Some(Utc::now());
            repo.sync_error = None;
        }
        // Guarded: a webhook may be recording a delivery at the same time.
        let (_, guard) = core.get_json_guarded::<Repo>(&auto::repo_path(&u.id)).await?;
        core.commit(
            vec![(auto::repo_path(&u.id), js(&repo)?), (auto::secrets_path(&u.id), js(&secrets)?)],
            used_key.into_iter().collect(),
            vec![guard],
        )
        .await?;
        Ok(reply(self.repo_reply(&repo, &me).await?, &me))
    }

    async fn delete_repo(&self, req: Request<pb::RepoRef>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let id = req.get_ref().id.clone();
        let repo = auto::get_repo(core, &id).await?;
        crate::audit::target(format!("repository {}", repo.name));
        for r in auto::list_runs(core, Some(&id)).await? {
            if !r.finished() && r.shown_status() != "lost" {
                return Err(Status::failed_precondition(format!("{} is still running — cancel it first", r.project_name)));
            }
        }
        for r in auto::list_runs(core, Some(&id)).await? {
            auto::delete_run_data(core, &r.id).await?;
        }
        let mut deletes = vec![auto::repo_path(&id), auto::secrets_path(&id)];
        for s in schedule::list(core, Some(&id)).await? {
            deletes.push(auto::schedule_path(&s.id));
        }
        deletes.extend(core.list(&format!("automation/state/{id}/")).await?);
        deletes.extend(core.list(&format!("automation/state-prev/{id}/")).await?);
        core.commit(vec![], deletes, vec![]).await?;
        git::remove(&id);
        Ok(reply((), &me))
    }

    async fn sync_repo(&self, req: Request<pb::RepoRef>) -> Reply<pb::Repo> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = req.get_ref().id.clone();
        crate::audit::target(format!("repository {id} (pull)"));
        let repo = auto::sync_repo(&self.st.core, &id).await?;
        Ok(reply(self.repo_reply(&repo, &me).await?, &me))
    }

    async fn check_runner(&self, req: Request<pb::CheckRunnerRequest>) -> Reply<pb::RunnerStatus> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.get_ref();
        crate::audit::target(format!("runner {}@{}", r.account, r.asset));
        self.check_runner_choice(&r.asset, &r.account).await?;
        let asset = bastion::get_asset(&self.st.core, &r.asset).await?;
        let status = match runner::check(&self.st.core, &asset, &r.account).await {
            Ok(tools) => pb::RunnerStatus { ok: true, error: String::new(), tools: tools.into_iter().map(|(name, version)| pb::Tool { name, version }).collect() },
            Err(e) => pb::RunnerStatus { ok: false, error: e, tools: vec![] },
        };
        Ok(reply(status, &me))
    }

    async fn list_runs(&self, req: Request<pb::ListRunsRequest>) -> Reply<pb::ListRunsResponse> {
        let (me, _) = self.who(&req, Need::Read).await?;
        let r = req.get_ref();
        let limit = if r.limit == 0 { 50 } else { r.limit.min(500) } as usize;
        let repo = (!r.repo.is_empty()).then_some(r.repo.as_str());
        let runs = auto::list_runs(&self.st.core, repo).await?.iter().take(limit).map(run_pb).collect();
        Ok(reply(pb::ListRunsResponse { runs }, &me))
    }

    async fn get_run(&self, req: Request<pb::RunRef>) -> Reply<pb::Run> {
        let (me, _) = self.who(&req, Need::Read).await?;
        let run = auto::get_run(&self.st.core, &req.get_ref().id).await?;
        Ok(reply(run_pb(&run), &me))
    }

    async fn start_run(&self, req: Request<pb::StartRunRequest>) -> Reply<pb::Run> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let repo = auto::get_repo(&self.st.core, &r.repo).await?;
        crate::audit::target(format!("{} {} in {}", r.project, r.action, repo.name));
        let who = who_name(&me);
        let run = auto::start_run(
            &self.st.core,
            &repo,
            Start { project: &r.project, action: &r.action, user: &who, trigger: "manual", plan_run: r.plan_run, options: opts_from(r.options) },
        )
        .await?;
        crate::audit::target(format!("{} {} in {} (run {})", run.project_name, run.action, repo.name, run.id));
        Ok(reply(run_pb(&run), &me))
    }

    async fn cancel_run(&self, req: Request<pb::RunRef>) -> Reply<pb::Run> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = req.get_ref().id.clone();
        crate::audit::target(format!("run {id} (cancel)"));
        let run = auto::get_run(&self.st.core, &id).await?;
        if run.finished() {
            return Ok(reply(run_pb(&run), &me));
        }
        // Lost runs have no instance left to stop them: close them here.
        let lost = run.shown_status() == "lost";
        let run = auto::update_run(&self.st.core, &id, |r| {
            r.cancel = true;
            if lost {
                r.status = "cancelled".into();
                r.finished_at = Some(Utc::now());
                r.error = Some("the instance running it stopped".into());
            }
        })
        .await?;
        Ok(reply(run_pb(&run), &me))
    }

    async fn approve_run(&self, req: Request<pb::RunRef>) -> Reply<pb::Run> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = req.get_ref().id.clone();
        crate::audit::target(format!("run {id} (approve)"));
        let run = auto::approve_run(&self.st.core, &id, &who_name(&me)).await?;
        Ok(reply(run_pb(&run), &me))
    }

    async fn reject_run(&self, req: Request<pb::RunRef>) -> Reply<pb::Run> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = req.get_ref().id.clone();
        crate::audit::target(format!("run {id} (reject)"));
        let run = auto::reject_run(&self.st.core, &id, &who_name(&me)).await?;
        Ok(reply(run_pb(&run), &me))
    }

    async fn list_schedules(&self, req: Request<pb::ListSchedulesRequest>) -> Reply<pb::ListSchedulesResponse> {
        let (me, _) = self.who(&req, Need::Read).await?;
        let repo = (!req.get_ref().repo.is_empty()).then(|| req.get_ref().repo.clone());
        let mut names = std::collections::HashMap::new();
        for r in auto::list_repos(&self.st.core).await? {
            for p in r.projects {
                names.insert((r.id.clone(), p.id), p.name);
            }
        }
        let schedules = schedule::list(&self.st.core, repo.as_deref())
            .await?
            .iter()
            .map(|s| schedule_pb(s, names.get(&(s.repo.clone(), s.project.clone())).cloned().unwrap_or_else(|| s.project.clone())))
            .collect();
        Ok(reply(pb::ListSchedulesResponse { schedules }, &me))
    }

    async fn save_schedule(&self, req: Request<pb::ScheduleInput>) -> Reply<pb::Schedule> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let i = req.into_inner();
        let repo = auto::get_repo(core, &i.repo).await?;
        let project = repo.projects.iter().find(|p| p.id == i.project).ok_or_else(|| AppError::NotFound(format!("project `{}`", i.project)))?;
        if !auto::actions(&project.kind).contains(&i.action.as_str()) {
            return Err(AppError::BadRequest(format!("{} projects can {}", project.kind, auto::actions(&project.kind).join(" or "))).into());
        }
        if i.action == "apply" {
            return Err(AppError::BadRequest("Apply needs a reviewed plan — schedule Plan instead (drift shows as changes)".into()).into());
        }
        auto::cron::Cron::parse(&i.cron).map_err(AppError::BadRequest)?;
        auto::cron::parse_offset(&i.timezone).map_err(AppError::BadRequest)?;
        let options = opts_from(i.options).cleaned(&project.kind);
        options.validate(&project.kind).map_err(AppError::BadRequest)?;
        let name = if i.name.trim().is_empty() { format!("{} {}", i.action, project.name) } else { i.name.trim().chars().take(100).collect() };
        let tz = if i.timezone.trim().is_empty() { "UTC".to_string() } else { i.timezone.trim().to_string() };
        crate::audit::target(format!("schedule “{name}” in {} ({})", repo.name, i.cron.trim()));
        let s = if i.id.is_empty() {
            Schedule {
                id: bastion::random_id(6),
                repo: repo.id.clone(),
                project: project.id.clone(),
                action: i.action.clone(),
                name,
                cron: i.cron.trim().to_string(),
                timezone: tz,
                options,
                enabled: i.enabled,
                created_by: who_name(&me),
                created_at: Utc::now(),
                last_fired: None,
                last_run: None,
                last_result: None,
            }
        } else {
            let mut s: Schedule = core.get_json(&auto::schedule_path(&i.id)).await?.ok_or_else(|| AppError::NotFound("schedule".into()))?;
            let timing = s.cron != i.cron.trim() || s.timezone != tz || (!s.enabled && i.enabled);
            s.project = project.id.clone();
            s.action = i.action.clone();
            s.name = name;
            s.cron = i.cron.trim().to_string();
            s.timezone = tz;
            s.options = options;
            s.enabled = i.enabled;
            if timing {
                // New timing: count from now, don't catch up.
                s.last_fired = Some(Utc::now());
            }
            s
        };
        core.commit(vec![(auto::schedule_path(&s.id), js(&s)?)], vec![], vec![]).await?;
        Ok(reply(schedule_pb(&s, project.name.clone()), &me))
    }

    async fn delete_schedule(&self, req: Request<pb::ScheduleRef>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = req.get_ref().id.clone();
        crate::audit::target(format!("schedule {id}"));
        self.st.core.commit(vec![], vec![auto::schedule_path(&id)], vec![]).await?;
        Ok(reply((), &me))
    }

    async fn run_schedule_now(&self, req: Request<pb::ScheduleRef>) -> Reply<pb::Run> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = req.get_ref().id.clone();
        let s: Schedule = self.st.core.get_json(&auto::schedule_path(&id)).await?.ok_or_else(|| AppError::NotFound("schedule".into()))?;
        crate::audit::target(format!("schedule “{}” (run now)", s.name));
        let run = schedule::fire(&self.st.core, &s, &who_name(&me)).await?;
        Ok(reply(run_pb(&run), &me))
    }

    async fn set_repo_policy(&self, req: Request<pb::RepoPolicy>) -> Reply<pb::Repo> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let p = req.into_inner();
        crate::audit::target(format!("repository {} (approval {})", p.id, if p.require_approval { "required" } else { "off" }));
        let repo = auto::update_repo(&self.st.core, &p.id, |r| r.require_approval = p.require_approval).await?;
        Ok(reply(self.repo_reply(&repo, &me).await?, &me))
    }

    async fn save_notifiers(&self, req: Request<pb::SaveNotifiersRequest>) -> Reply<pb::Repo> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let core = &self.st.core;
        let r = req.into_inner();
        let repo = auto::get_repo(core, &r.repo).await?;
        crate::audit::target(format!("repository {} (notifications)", repo.name));
        let (secrets, sguard) = core.get_json_guarded::<RepoSecrets>(&auto::secrets_path(&r.repo)).await?;
        let mut secrets = secrets.unwrap_or_default();
        let mut list = Vec::new();
        let mut targets = std::collections::HashMap::new();
        if r.notifiers.len() > 20 {
            return Err(AppError::BadRequest("at most 20 notifications".into()).into());
        }
        for n in r.notifiers {
            let id = if n.id.is_empty() { bastion::random_id(4) } else { n.id.clone() };
            let target = if n.url.trim().is_empty() {
                secrets.notify.get(&id).cloned().ok_or_else(|| AppError::BadRequest(format!("{}: enter the URL", n.kind)))?
            } else {
                NotifyTarget { url: n.url.trim().to_string(), chat_id: n.chat_id.trim().to_string() }
            };
            let label = notify::check(&n.kind, &target).map_err(AppError::BadRequest)?;
            let on: Vec<String> = n.on.into_iter().filter(|e| notify::EVENTS.contains(&e.as_str())).collect();
            if on.is_empty() {
                return Err(AppError::BadRequest(format!("{label}: choose when to notify")).into());
            }
            targets.insert(id.clone(), target);
            list.push(Notifier { id, kind: n.kind, on, label });
        }
        secrets.notify = targets;
        let (repo_now, rguard) = core.get_json_guarded::<Repo>(&auto::repo_path(&r.repo)).await?;
        let mut repo_now = repo_now.ok_or_else(|| AppError::NotFound("repository".into()))?;
        repo_now.notifiers = list;
        core.commit(
            vec![(auto::repo_path(&r.repo), js(&repo_now)?), (auto::secrets_path(&r.repo), js(&secrets)?)],
            vec![],
            vec![rguard, sguard],
        )
        .await?;
        Ok(reply(self.repo_reply(&repo_now, &me).await?, &me))
    }

    async fn test_notifier(&self, req: Request<pb::TestNotifierRequest>) -> Reply<pb::TestNotifierResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let n = r.notifier.ok_or_else(|| AppError::BadRequest("missing notifier".into()))?;
        let repo = auto::get_repo(&self.st.core, &r.repo).await?;
        crate::audit::target(format!("repository {} (test notification)", repo.name));
        let target = if n.url.trim().is_empty() {
            auto::get_secrets(&self.st.core, &r.repo).await?.notify.get(&n.id).cloned().ok_or_else(|| AppError::BadRequest("enter the URL".into()))?
        } else {
            NotifyTarget { url: n.url.trim().to_string(), chat_id: n.chat_id.trim().to_string() }
        };
        notify::check(&n.kind, &target).map_err(AppError::BadRequest)?;
        let text = format!("✓ timika test notification for {} — this is where run alerts will arrive.", repo.name);
        let res = notify::send(&n.kind, &target, "test", &text, None).await;
        Ok(reply(pb::TestNotifierResponse { ok: res.is_ok(), error: res.err().unwrap_or_default() }, &me))
    }

    async fn rotate_trigger_token(&self, req: Request<pb::RepoRef>) -> Reply<pb::Repo> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let id = req.get_ref().id.clone();
        let core = &self.st.core;
        let repo = auto::get_repo(core, &id).await?;
        crate::audit::target(format!("repository {} (new CI token)", repo.name));
        for _ in 0..crate::core::MAX_RETRIES {
            let (s, guard) = core.get_json_guarded::<RepoSecrets>(&auto::secrets_path(&id)).await?;
            let mut s = s.unwrap_or_default();
            s.trigger_token = format!("tmkci.{}", bastion::random_id(24));
            match core.commit(vec![(auto::secrets_path(&id), js(&s)?)], vec![], vec![guard]).await {
                Err(AppError::WriteConflict) => continue,
                other => {
                    other?;
                    break;
                }
            }
        }
        Ok(reply(self.repo_reply(&repo, &me).await?, &me))
    }

    async fn list_repo_files(&self, req: Request<pb::RepoFilesRequest>) -> Reply<pb::RepoFilesResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let (repo, sha) = self.repo_at(&r.repo, &r.sha).await?;
        let path = git::check_path(&r.path).map_err(AppError::BadRequest)?;
        crate::audit::target(format!("repository {} (browse /{path})", repo.name));
        let node = git::browse(&repo.id, &sha, &path).await.map_err(AppError::BadRequest)?;
        let (kind, entries) = match node {
            git::Node::File => ("file", vec![]),
            git::Node::Dir(es) => ("dir", es.into_iter().map(|e| pb::RepoFileEntry { name: e.name, kind: e.kind, size: e.size }).collect()),
        };
        Ok(reply(pb::RepoFilesResponse { path, sha, kind: kind.into(), entries }, &me))
    }

    async fn read_repo_file(&self, req: Request<pb::RepoFileRequest>) -> Reply<pb::RepoFile> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let (repo, sha) = self.repo_at(&r.repo, &r.sha).await?;
        let path = git::check_path(&r.path).map_err(AppError::BadRequest)?;
        crate::audit::target(format!("repository {} (read /{path} at {})", repo.name, &sha[..7]));
        let b = git::read_blob(&repo.id, &sha, &path).await.map_err(AppError::BadRequest)?;
        Ok(reply(pb::RepoFile { path, sha, size: b.size, binary: b.binary, too_large: b.too_large, content: b.text }, &me))
    }

    type WatchRunStream = EventStream;

    /// Output from the start, then every new chunk until the run ends. Reads
    /// storage, so it works from any instance.
    async fn watch_run(&self, req: Request<pb::RunRef>) -> Reply<Self::WatchRunStream> {
        let (me, _) = self.who(&req, Need::Read).await?;
        let id = req.get_ref().id.clone();
        let core = self.st.core.clone();
        let first = auto::get_run(&core, &id).await?;
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<pb::RunEvent, Status>>(16);
        tokio::spawn(async move {
            let mut seq = 0u32;
            let mut last_status = String::new();
            let mut run = first;
            loop {
                let finished = run.finished() || run.shown_status() == "lost";
                let status = run.shown_status().to_string();
                if status != last_status {
                    last_status = status;
                    if tx.send(Ok(pb::RunEvent { output: String::new(), run: Some(run_pb(&run)) })).await.is_err() {
                        return;
                    }
                }
                // Everything stored so far (the final status is written after the last chunk).
                loop {
                    match core.get(&auto::log_chunk(&id, seq)).await {
                        Ok(Some(data)) => {
                            seq += 1;
                            let ev = pb::RunEvent { output: String::from_utf8_lossy(&data).into_owned(), run: None };
                            if tx.send(Ok(ev)).await.is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            let _ = tx.send(Err(e.into())).await;
                            return;
                        }
                    }
                }
                if finished {
                    let _ = tx.send(Ok(pb::RunEvent { output: String::new(), run: Some(run_pb(&run)) })).await;
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                match auto::get_run(&core, &id).await {
                    Ok(r) => run = r,
                    Err(e) => {
                        let _ = tx.send(Err(e.into())).await;
                        return;
                    }
                }
            }
        });
        Ok(reply(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)) as EventStream, &me))
    }
}

#[allow(dead_code)]
fn _keep(_: Response<()>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(name_from_url("https://github.com/acme/infra.git"), "acme/infra");
        assert_eq!(name_from_url("git@github.com:acme/infra.git"), "acme/infra");
        assert_eq!(name_from_url("ssh://git@gitlab.example.com/group/sub/infra"), "sub/infra");
        assert_eq!(name_from_url("file:///tmp/x/infra"), "x/infra");
    }
}
