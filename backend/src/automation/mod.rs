//! Automation: infrastructure as code from Git.
//!
//! A repository is cloned (bare) on the timika instance; its Terraform /
//! OpenTofu, Ansible and Pulumi projects are detected from the file tree. A run
//! sends a snapshot of one commit (`git archive`) to a **runner server** — one
//! of the bastion's servers — over SFTP and executes the tool there over SSH.
//! Code from the repository never runs on the vault host.
//!
//! Storage (barrier-encrypted, like everything else):
//!   automation/repos/<id>           repository settings + last sync (no secrets)
//!   automation/secrets/<id>         token / deploy key / webhook secret / secret variables
//!   automation/deploy-keys/<id>     a generated key pair not yet used by a repository (1 h)
//!   automation/runs/<id>            run metadata
//!   automation/logs/<id>/<seq>      run output (secrets masked), in order
//!   automation/schedules/<id>       cron schedules (repo, project, action, options)
//!   automation/plans/<id>           a Terraform plan file, for "apply this plan"
//!   automation/state/<repo>/<p>     terraform.tfstate of a project with no backend
//!                                   (plus state-prev/…: the version before the last apply)

pub mod cron;
pub mod git;
pub mod notify;
pub mod runner;
pub mod schedule;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::{Core, MAX_RETRIES};
use crate::error::{AppError, AppResult};

pub const REPOS: &str = "automation/repos/";
pub const RUNS: &str = "automation/runs/";

pub fn repo_path(id: &str) -> String {
    format!("{REPOS}{id}")
}
pub fn secrets_path(id: &str) -> String {
    format!("automation/secrets/{id}")
}
pub fn deploy_key_path(id: &str) -> String {
    format!("automation/deploy-keys/{id}")
}
pub fn run_path(id: &str) -> String {
    format!("{RUNS}{id}")
}
pub fn log_prefix(id: &str) -> String {
    format!("automation/logs/{id}/")
}
pub fn log_chunk(id: &str, seq: u32) -> String {
    format!("automation/logs/{id}/{seq:08}")
}
pub const SCHEDULES: &str = "automation/schedules/";
pub fn schedule_path(id: &str) -> String {
    format!("{SCHEDULES}{id}")
}
pub fn plan_path(id: &str) -> String {
    format!("automation/plans/{id}")
}
fn project_key(project: &str) -> String {
    use sha2::Digest;
    hex::encode(&sha2::Sha256::digest(project.as_bytes())[..12])
}
pub fn state_path(repo: &str, project: &str) -> String {
    format!("automation/state/{repo}/{}", project_key(project))
}
pub fn prev_state_path(repo: &str, project: &str) -> String {
    format!("automation/state-prev/{repo}/{}", project_key(project))
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Commit {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub time: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Project {
    pub id: String,
    pub kind: String,
    pub dir: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub stack: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Variable {
    pub name: String,
    /// None for secrets (the value is in `RepoSecrets.vars`).
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HookDelivery {
    pub time: DateTime<Utc>,
    pub sha: String,
    pub by: String,
    pub result: String,
}

/// Per-run choices (Semaphore calls them prompts). Every field is optional;
/// the ones that don't apply to a project's kind are dropped.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct RunOptions {
    /// Ansible --limit
    #[serde(default)]
    pub limit: String,
    #[serde(default)]
    pub tags: String,
    #[serde(default)]
    pub skip_tags: String,
    /// Ansible extra vars: `key=value` lines or a JSON object.
    #[serde(default)]
    pub extra_vars: String,
    /// Ansible -v … -vvvv
    #[serde(default)]
    pub verbose: u8,
    /// Ansible: run against timika's servers with these tags (comma-separated, `*` = all)
    /// instead of the repository's inventory.
    #[serde(default)]
    pub servers: String,
    /// Terraform: a destroy plan.
    #[serde(default)]
    pub destroy: bool,
    /// Terraform -target / Pulumi --target
    #[serde(default)]
    pub targets: Vec<String>,
    /// Terraform -replace
    #[serde(default)]
    pub replace: Vec<String>,
    /// Terraform workspace (created if missing).
    #[serde(default)]
    pub workspace: String,
    /// Pulumi --refresh
    #[serde(default)]
    pub refresh: bool,
}

fn plain(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(|c| c.is_control())
}

impl RunOptions {
    /// Only the fields that mean something for `kind`.
    pub fn cleaned(&self, kind: &str) -> Self {
        let mut o = self.clone();
        let trim = |v: &mut Vec<String>| {
            *v = v.iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        };
        trim(&mut o.targets);
        trim(&mut o.replace);
        o.limit = o.limit.trim().into();
        o.tags = o.tags.trim().into();
        o.skip_tags = o.skip_tags.trim().into();
        o.workspace = o.workspace.trim().into();
        o.servers = o.servers.trim().into();
        if kind != "ansible" {
            (o.limit, o.tags, o.skip_tags, o.extra_vars, o.verbose, o.servers) = Default::default();
        }
        if kind != "terraform" {
            (o.destroy, o.replace, o.workspace) = Default::default();
        }
        if kind != "pulumi" {
            o.refresh = false;
        }
        if kind == "ansible" {
            o.targets.clear();
        }
        o
    }

    pub fn validate(&self, _kind: &str) -> Result<(), String> {
        for (name, v) in [("limit", &self.limit), ("tags", &self.tags), ("skip tags", &self.skip_tags)] {
            if !plain(v, 500) {
                return Err(format!("{name} is too long or has control characters"));
            }
        }
        if self.verbose > 4 {
            return Err("verbosity is 0–4".into());
        }
        if self.targets.len() > 50 || self.replace.len() > 50 || self.targets.iter().chain(&self.replace).any(|t| !plain(t, 500)) {
            return Err("too many or invalid targets".into());
        }
        if !self.workspace.is_empty() && (self.workspace.len() > 64 || !self.workspace.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))) {
            return Err("a workspace name is letters, digits, - and _".into());
        }
        if !self.servers.is_empty() && (self.servers.len() > 300 || !self.servers.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ',' | ' ' | '*'))) {
            return Err("servers: tags separated by commas, or *".into());
        }
        self.extra_json()?;
        Ok(())
    }

    /// Extra vars as a JSON object (None when empty).
    pub fn extra_json(&self) -> Result<Option<String>, String> {
        let t = self.extra_vars.trim();
        if t.is_empty() {
            return Ok(None);
        }
        if t.len() > 64 * 1024 {
            return Err("extra vars are limited to 64 KB".into());
        }
        if t.starts_with('{') {
            let v: serde_json::Value = serde_json::from_str(t).map_err(|e| format!("extra vars: invalid JSON ({e})"))?;
            if !v.is_object() {
                return Err("extra vars: a JSON object".into());
            }
            return Ok(Some(v.to_string()));
        }
        let mut m = serde_json::Map::new();
        for line in t.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (k, v) = line.split_once('=').ok_or_else(|| format!("extra vars: `{line}` is not key=value"))?;
            let k = k.trim();
            if !valid_var_name(k) {
                return Err(format!("extra vars: `{k}` is not a valid name"));
            }
            m.insert(k.to_string(), serde_json::Value::String(v.trim().to_string()));
        }
        Ok(Some(serde_json::Value::Object(m).to_string()))
    }

    /// "limit web · tags deploy · destroy" — for lists and notifications.
    pub fn describe(&self) -> String {
        let mut p = Vec::new();
        if self.destroy {
            p.push("destroy".to_string());
        }
        if !self.workspace.is_empty() {
            p.push(format!("workspace {}", self.workspace));
        }
        if !self.servers.is_empty() {
            p.push(format!("servers {}", self.servers));
        }
        if !self.limit.is_empty() {
            p.push(format!("limit {}", self.limit));
        }
        if !self.tags.is_empty() {
            p.push(format!("tags {}", self.tags));
        }
        if !self.skip_tags.is_empty() {
            p.push(format!("skip {}", self.skip_tags));
        }
        if !self.extra_vars.trim().is_empty() {
            p.push("extra vars".into());
        }
        if self.verbose > 0 {
            p.push(format!("-{}", "v".repeat(self.verbose as usize)));
        }
        for t in &self.targets {
            p.push(format!("target {t}"));
        }
        for t in &self.replace {
            p.push(format!("replace {t}"));
        }
        if self.refresh {
            p.push("refresh".into());
        }
        p.join(" · ")
    }
}

/// Where run events are sent. The target (URL / bot token) is a secret.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Notifier {
    pub id: String,
    /// slack · teams · discord · telegram · webhook
    pub kind: String,
    /// failed · drift · approval · succeeded
    pub on: Vec<String>,
    /// Shown instead of the secret target, e.g. "hooks.slack.com/…/x9Kd".
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct NotifyTarget {
    pub url: String,
    #[serde(default)]
    pub chat_id: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Repo {
    pub id: String,
    pub name: String,
    pub url: String,
    pub branch: String,
    /// none · token · deploy_key
    pub auth: String,
    #[serde(default)]
    pub deploy_public_key: Option<String>,
    pub runner_asset: String,
    pub runner_account: String,
    #[serde(default)]
    pub auto_plan: bool,
    /// auto · terraform · tofu
    #[serde(default = "auto")]
    pub tf_bin: String,
    #[serde(default)]
    pub variables: Vec<Variable>,
    #[serde(default)]
    pub head: Option<Commit>,
    #[serde(default)]
    pub synced_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sync_error: Option<String>,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub last_hook: Option<HookDelivery>,
    /// Apply / Run / Up wait for another admin's approval.
    #[serde(default)]
    pub require_approval: bool,
    #[serde(default)]
    pub notifiers: Vec<Notifier>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

fn auto() -> String {
    "auto".into()
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct RepoSecrets {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub private_key: Option<String>,
    pub webhook_secret: String,
    /// Secret variable values by name.
    #[serde(default)]
    pub vars: HashMap<String, String>,
    /// Bearer token for `POST /v1/automation/trigger/<repo>` (CI).
    #[serde(default)]
    pub trigger_token: String,
    /// Notifier id → where to send.
    #[serde(default)]
    pub notify: HashMap<String, NotifyTarget>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PendingKey {
    pub private_key: String,
    pub public_key: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Run {
    pub id: String,
    pub repo: String,
    pub repo_name: String,
    pub project: String,
    pub project_name: String,
    pub kind: String,
    pub action: String,
    /// approval · queued · running · succeeded · failed · cancelled
    pub status: String,
    pub user: String,
    /// manual · push · schedule · ci
    pub trigger: String,
    pub sha: String,
    pub commit_message: String,
    pub runner_asset: String,
    pub runner_account: String,
    pub runner_name: String,
    pub instance: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    pub heartbeat_at: DateTime<Utc>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub changes: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub plan_run: Option<String>,
    #[serde(default)]
    pub applied_by: Option<String>,
    /// Set by CancelRun; the instance running it stops it within ~1 s.
    #[serde(default)]
    pub cancel: bool,
    #[serde(default)]
    pub options: RunOptions,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub log_chunks: u32,
    #[serde(default)]
    pub log_bytes: u64,
    #[serde(default)]
    pub truncated: bool,
}

impl Run {
    pub fn finished(&self) -> bool {
        matches!(self.status.as_str(), "succeeded" | "failed" | "cancelled")
    }
    /// Running on an instance that stopped heartbeating → `lost`.
    pub fn shown_status(&self) -> &str {
        if !self.finished() && self.status != "approval" && (Utc::now() - self.heartbeat_at).num_seconds() > 30 {
            "lost"
        } else {
            &self.status
        }
    }
}

/// The actions a kind supports; the first is the safe one (no changes made).
pub fn actions(kind: &str) -> &'static [&'static str] {
    match kind {
        "terraform" => &["plan", "apply"],
        "ansible" => &["check", "run"],
        "pulumi" => &["preview", "up"],
        _ => &[],
    }
}

pub fn safe_action(kind: &str) -> &'static str {
    actions(kind).first().copied().unwrap_or("plan")
}

pub fn valid_var_name(n: &str) -> bool {
    let mut c = n.chars();
    matches!(c.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_') && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') && n.len() <= 128
}

// ── storage helpers ─────────────────────────────────────────────────────────

pub async fn get_repo(core: &Core, id: &str) -> AppResult<Repo> {
    core.get_json(&repo_path(id)).await?.ok_or_else(|| AppError::NotFound(format!("repository `{id}`")))
}

pub async fn get_secrets(core: &Core, id: &str) -> AppResult<RepoSecrets> {
    Ok(core.get_json(&secrets_path(id)).await?.unwrap_or_default())
}

pub async fn list_repos(core: &Core) -> AppResult<Vec<Repo>> {
    let mut out = Vec::new();
    for key in core.list(REPOS).await? {
        if let Some(r) = core.get_json::<Repo>(&key).await? {
            out.push(r);
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

pub async fn get_run(core: &Core, id: &str) -> AppResult<Run> {
    core.get_json(&run_path(id)).await?.ok_or_else(|| AppError::NotFound(format!("run `{id}`")))
}

/// Newest first.
pub async fn list_runs(core: &Core, repo: Option<&str>) -> AppResult<Vec<Run>> {
    let mut out = Vec::new();
    for key in core.list(RUNS).await? {
        if let Some(r) = core.get_json::<Run>(&key).await? {
            if repo.is_none_or(|x| x == r.repo) {
                out.push(r);
            }
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// Read-modify-write a run (guarded, retried — other instances write it too).
pub async fn update_run(core: &Core, id: &str, f: impl Fn(&mut Run)) -> AppResult<Run> {
    for _ in 0..MAX_RETRIES {
        let (run, guard) = core.get_json_guarded::<Run>(&run_path(id)).await?;
        let Some(mut run) = run else { return Err(AppError::NotFound(format!("run `{id}`"))) };
        f(&mut run);
        match core.commit(vec![(run_path(id), serde_json::to_vec(&run)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            Err(e) => return Err(e),
            Ok(()) => return Ok(run),
        }
    }
    Err(AppError::WriteConflict)
}

/// Read-modify-write a repository.
pub async fn update_repo(core: &Core, id: &str, f: impl Fn(&mut Repo)) -> AppResult<Repo> {
    for _ in 0..MAX_RETRIES {
        let (repo, guard) = core.get_json_guarded::<Repo>(&repo_path(id)).await?;
        let Some(mut repo) = repo else { return Err(AppError::NotFound(format!("repository `{id}`"))) };
        f(&mut repo);
        match core.commit(vec![(repo_path(id), serde_json::to_vec(&repo)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            Err(e) => return Err(e),
            Ok(()) => return Ok(repo),
        }
    }
    Err(AppError::WriteConflict)
}

/// Delete finished runs (with their output and plans) older than the retention.
pub async fn purge_old_runs(core: &Core, retention_days: i64) -> AppResult<usize> {
    let cutoff = Utc::now() - chrono::Duration::days(retention_days);
    let mut purged = 0;
    for r in list_runs(core, None).await? {
        if r.finished() && r.finished_at.unwrap_or(r.created_at) < cutoff {
            delete_run_data(core, &r.id).await?;
            purged += 1;
        }
    }
    Ok(purged)
}

pub async fn delete_run_data(core: &Core, id: &str) -> AppResult<()> {
    let mut deletes = core.list(&log_prefix(id)).await?;
    deletes.push(plan_path(id));
    deletes.push(run_path(id));
    core.commit(vec![], deletes, vec![]).await
}

// ── operations (API and webhook) ────────────────────────────────────────────

/// git pull: fetch the branch, detect projects, record the result.
pub async fn sync_repo(core: &Core, id: &str) -> AppResult<Repo> {
    let repo = get_repo(core, id).await?;
    let s = get_secrets(core, id).await?;
    let auth = git::Auth { token: s.token, private_key: s.private_key };
    match git::sync(id, &repo.url, &repo.branch, &auth).await {
        Ok(synced) => {
            update_repo(core, id, |r| {
                r.head = Some(synced.head.clone());
                r.projects = synced.projects.clone();
                r.synced_at = Some(Utc::now());
                r.sync_error = None;
            })
            .await
        }
        Err(e) => {
            let _ = update_repo(core, id, |r| {
                r.synced_at = Some(Utc::now());
                r.sync_error = Some(e.clone());
            })
            .await;
            Err(AppError::BadRequest(format!("pull failed: {e}")))
        }
    }
}

/// What to start.
pub struct Start<'a> {
    pub project: &'a str,
    pub action: &'a str,
    pub user: &'a str,
    /// manual · push · schedule · ci
    pub trigger: &'a str,
    pub plan_run: Option<String>,
    pub options: RunOptions,
}

/// Does this action change infrastructure (apply · run · up)?
pub fn changes_things(kind: &str, action: &str) -> bool {
    actions(kind).get(1) == Some(&action)
}

/// Queue a run and start it on this instance — or, when the repository needs
/// approval for changes, park it until another admin approves.
pub async fn start_run(core: &std::sync::Arc<Core>, repo: &Repo, req: Start<'_>) -> AppResult<Run> {
    let Start { project: project_id, action, user, trigger, plan_run, options } = req;
    let project = repo.projects.iter().find(|p| p.id == project_id).ok_or_else(|| AppError::NotFound(format!("project `{project_id}` — pull to refresh")))?;
    if !actions(&project.kind).contains(&action) {
        return Err(AppError::BadRequest(format!("{} projects can {}", project.kind, actions(&project.kind).join(" or "))));
    }
    let mut options = options.cleaned(&project.kind);
    options.validate(&project.kind).map_err(AppError::BadRequest)?;
    let busy = list_runs(core, Some(&repo.id)).await?.into_iter().find(|r| r.project == project.id && matches!(r.shown_status(), "queued" | "running" | "approval"));
    if let Some(b) = busy {
        let what = if b.status == "approval" { "waiting for approval" } else { "running" };
        return Err(AppError::Conflict(format!("{} already has a {} {what} ({}) — wait for it or cancel it", project.name, b.action, b.id)));
    }
    let asset = crate::bastion::get_asset(core, &repo.runner_asset).await.map_err(|_| AppError::BadRequest("the runner server no longer exists — choose another in the repository settings".into()))?;
    let (sha, message) = if action == "apply" {
        let pid = plan_run.clone().ok_or_else(|| AppError::BadRequest("apply needs a plan: run Plan first, then apply it".into()))?;
        let plan = get_run(core, &pid).await?;
        if plan.repo != repo.id || plan.project != project.id || plan.action != "plan" {
            return Err(AppError::BadRequest("that plan is for another project".into()));
        }
        if plan.status != "succeeded" {
            return Err(AppError::BadRequest("only a successful plan can be applied".into()));
        }
        if plan.changes != Some(true) {
            return Err(AppError::BadRequest("that plan has no changes to apply".into()));
        }
        if let Some(a) = &plan.applied_by {
            return Err(AppError::Conflict(format!("that plan was already applied (run {a}) — plan again")));
        }
        if core.get(&plan_path(&pid)).await?.is_none() {
            return Err(AppError::BadRequest("the plan file is gone — plan again".into()));
        }
        // The plan file decides what happens; its workspace must match.
        options = plan.options.clone();
        (plan.sha, plan.commit_message)
    } else {
        let head = repo.head.clone().ok_or_else(|| AppError::BadRequest("the repository hasn't been pulled yet".into()))?;
        (head.sha, head.message)
    };
    let mut run = runner::new_run(repo, project, action, user, trigger, &sha, &message, plan_run.clone());
    run.runner_name = format!("{}@{}", repo.runner_account, asset.name);
    run.options = options;
    let parked = repo.require_approval && changes_things(&project.kind, action);
    if parked {
        run.status = "approval".into();
    }
    if let Some(pid) = &plan_run {
        // Claim the plan (once): a second Apply of the same plan is refused.
        let rid = run.id.clone();
        let mut claimed = false;
        for _ in 0..MAX_RETRIES {
            let (plan, guard) = core.get_json_guarded::<Run>(&run_path(pid)).await?;
            let Some(mut plan) = plan else { return Err(AppError::NotFound("plan".into())) };
            if let Some(a) = &plan.applied_by {
                return Err(AppError::Conflict(format!("that plan was already applied (run {a}) — plan again")));
            }
            plan.applied_by = Some(rid.clone());
            match core
                .commit(vec![(run_path(pid), serde_json::to_vec(&plan)?), (run_path(&run.id), serde_json::to_vec(&run)?)], vec![], vec![guard])
                .await
            {
                Err(AppError::WriteConflict) => continue,
                Err(e) => return Err(e),
                Ok(()) => {
                    claimed = true;
                    break;
                }
            }
        }
        if !claimed {
            return Err(AppError::WriteConflict);
        }
    } else {
        core.commit(vec![(run_path(&run.id), serde_json::to_vec(&run)?)], vec![], vec![]).await?;
    }
    if parked {
        notify::run_event(core, &run, "approval").await;
    } else {
        tokio::spawn(runner::execute(core.clone(), run.id.clone()));
    }
    Ok(run)
}

/// Approve a parked run: another admin than the one who asked.
pub async fn approve_run(core: &std::sync::Arc<Core>, id: &str, who: &str) -> AppResult<Run> {
    let run = get_run(core, id).await?;
    if run.status != "approval" {
        return Err(AppError::BadRequest(format!("this run is {}, not waiting for approval", run.status)));
    }
    if run.user == who {
        return Err(AppError::Forbidden("someone else must approve your own run".into()));
    }
    let who = who.to_string();
    let run = update_run(core, id, |r| {
        if r.status == "approval" {
            r.status = "queued".into();
            r.approved_by = Some(who.clone());
            r.heartbeat_at = Utc::now();
        }
    })
    .await?;
    if run.approved_by.as_deref() != Some(who.as_str()) || run.status != "queued" {
        return Err(AppError::Conflict("someone else decided on this run meanwhile".into()));
    }
    tokio::spawn(runner::execute(core.clone(), run.id.clone()));
    Ok(run)
}

/// Reject (or withdraw) a parked run. A plan it was going to apply can be applied again.
pub async fn reject_run(core: &Core, id: &str, who: &str) -> AppResult<Run> {
    let who = who.to_string();
    let run = update_run(core, id, |r| {
        if r.status == "approval" {
            r.status = "cancelled".into();
            r.finished_at = Some(Utc::now());
            r.error = Some(format!("rejected by {who}"));
        }
    })
    .await?;
    if let Some(pid) = &run.plan_run {
        if run.error.as_deref().is_some_and(|e| e.starts_with("rejected")) {
            let rid = run.id.clone();
            let _ = update_run(core, pid, |p| {
                if p.applied_by.as_deref() == Some(rid.as_str()) {
                    p.applied_by = None;
                }
            })
            .await;
        }
    }
    Ok(run)
}

// ── project detection ───────────────────────────────────────────────────────

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map(|(_, f)| f).unwrap_or(path)
}

/// Folders that hold reusable pieces, not something to run.
fn skipped_dir(dir: &str) -> bool {
    dir.split('/').any(|seg| {
        matches!(
            seg,
            "modules" | ".terraform" | ".github" | ".gitlab" | "node_modules" | "roles" | "group_vars" | "host_vars"
                | "tasks" | "handlers" | "templates" | "vars" | "defaults" | "meta" | "files" | "molecule" | "collections"
                | "venv" | ".venv" | "test" | "tests"
        )
    })
}

/// Does this YAML look like an Ansible playbook (a list of plays)?
pub fn looks_like_playbook(text: &str) -> bool {
    let mut first_item = None;
    let mut hosts = false;
    for line in text.lines() {
        let t = line.trim_end();
        if t.is_empty() || t.trim_start().starts_with('#') || t == "---" {
            continue;
        }
        if first_item.is_none() {
            first_item = Some(t.starts_with("- "));
        }
        let s = t.trim_start_matches("- ").trim_start();
        let top = t.starts_with("- ") || t.starts_with("  ") && !t.starts_with("   ");
        if top && (s.starts_with("hosts:") || s.starts_with("import_playbook:") || s.starts_with("ansible.builtin.import_playbook:")) {
            hosts = true;
        }
    }
    first_item == Some(true) && hosts
}

/// Projects in a repository's file list. `read` returns a file's text (for
/// telling playbooks from other YAML).
pub fn detect(files: &[String], mut read: impl FnMut(&str) -> Option<String>) -> Vec<Project> {
    let mut out: Vec<Project> = Vec::new();
    let mut tf_dirs: Vec<&str> = Vec::new();
    let mut checked = 0;
    for f in files {
        let dir = parent(f);
        let name = file_name(f);
        if skipped_dir(dir) || dir.split('/').any(|seg| seg.starts_with('.')) {
            continue;
        }
        if (name.ends_with(".tf") || name.ends_with(".tofu")) && !tf_dirs.contains(&dir) {
            tf_dirs.push(dir);
        }
        if name == "Pulumi.yaml" || name == "Pulumi.yml" {
            let stacks: Vec<&str> = files
                .iter()
                .filter(|g| parent(g) == dir)
                .filter_map(|g| {
                    let n = file_name(g);
                    n.strip_prefix("Pulumi.").and_then(|s| s.strip_suffix(".yaml").or_else(|| s.strip_suffix(".yml")))
                })
                .filter(|s| !s.is_empty() && !s.contains('.'))
                .collect();
            let label = if dir.is_empty() { "(root)" } else { dir };
            if stacks.is_empty() {
                out.push(Project { id: format!("pulumi:{dir}"), kind: "pulumi".into(), dir: dir.into(), file: String::new(), stack: String::new(), name: label.into() });
            }
            for s in stacks {
                out.push(Project { id: format!("pulumi:{dir}#{s}"), kind: "pulumi".into(), dir: dir.into(), file: String::new(), stack: s.into(), name: format!("{label} · {s}") });
            }
        }
        let yaml = name.ends_with(".yml") || name.ends_with(".yaml");
        if yaml && !name.starts_with("Pulumi.") && !name.starts_with('.') && checked < 300 && !matches!(name, "requirements.yml" | "requirements.yaml" | "galaxy.yml" | "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml") {
            checked += 1;
            if read(f).is_some_and(|t| looks_like_playbook(&t)) {
                out.push(Project { id: format!("ansible:{f}"), kind: "ansible".into(), dir: dir.into(), file: name.into(), stack: String::new(), name: f.clone() });
            }
        }
    }
    for d in tf_dirs {
        out.push(Project { id: format!("terraform:{d}"), kind: "terraform".into(), dir: d.into(), file: String::new(), stack: String::new(), name: if d.is_empty() { "(root)".into() } else { d.into() } });
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
    out.truncate(200);
    out
}

/// Projects a push touched (by changed files). An empty list = unknown → all.
pub fn touched<'a>(projects: &'a [Project], changed: &[String]) -> Vec<&'a Project> {
    if changed.is_empty() {
        return projects.iter().collect();
    }
    let inside = |f: &str, dir: &str| dir.is_empty() && !f.contains('/') || !dir.is_empty() && f.starts_with(&format!("{dir}/"));
    // Terraform / Pulumi folders: a change there is theirs, not a playbook's.
    let iac_dirs: Vec<&str> = projects.iter().filter(|p| p.kind != "ansible" && !p.dir.is_empty()).map(|p| p.dir.as_str()).collect();
    projects
        .iter()
        .filter(|p| {
            changed.iter().any(|f| match p.kind.as_str() {
                // A playbook pulls in roles / vars from around it: anything
                // under its folder that isn't another project's.
                "ansible" => (p.dir.is_empty() || f.starts_with(&format!("{}/", p.dir))) && !iac_dirs.iter().any(|d| f.starts_with(&format!("{d}/"))),
                _ => inside(f, &p.dir),
            })
        })
        .collect()
}

// ── output ──────────────────────────────────────────────────────────────────

fn num_before(s: &str, word: &str) -> Option<u64> {
    let i = s.find(word)?;
    s[..i].trim_end().rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// One line summarizing a finished run's output, and whether it found or made changes.
pub fn summarize(kind: &str, action: &str, out: &str) -> (Option<String>, Option<bool>) {
    match kind {
        "terraform" => {
            if let Some(line) = out.lines().rev().find(|l| l.contains("Plan:") && l.contains("to add")) {
                let (a, c, d) = (num_before(line, " to add").unwrap_or(0), num_before(line, " to change").unwrap_or(0), num_before(line, " to destroy").unwrap_or(0));
                if action == "plan" {
                    return (Some(format!("+{a} ~{c} -{d}")), Some(a + c + d > 0));
                }
            }
            if let Some(line) = out.lines().rev().find(|l| l.contains("Apply complete!")) {
                let (a, c, d) = (num_before(line, " added").unwrap_or(0), num_before(line, " changed").unwrap_or(0), num_before(line, " destroyed").unwrap_or(0));
                return (Some(format!("{a} added · {c} changed · {d} destroyed")), Some(a + c + d > 0));
            }
            if out.contains("No changes.") {
                return (Some("no changes".into()), Some(false));
            }
            (None, None)
        }
        "ansible" => {
            let Some(i) = out.rfind("PLAY RECAP") else { return (None, None) };
            let (mut hosts, mut ok, mut changed, mut failed, mut unreachable) = (0, 0, 0, 0, 0);
            for line in out[i..].lines().skip(1) {
                if !line.contains("ok=") {
                    if hosts > 0 {
                        break;
                    }
                    continue;
                }
                hosts += 1;
                for part in line.split_whitespace() {
                    let (k, v) = part.split_once('=').unwrap_or(("", ""));
                    let v: u64 = v.parse().unwrap_or(0);
                    match k {
                        "ok" => ok += v,
                        "changed" => changed += v,
                        "failed" => failed += v,
                        "unreachable" => unreachable += v,
                        _ => {}
                    }
                }
            }
            if hosts == 0 {
                return (None, None);
            }
            let mut s = format!("{hosts} host{} · ok {ok} · changed {changed}", if hosts == 1 { "" } else { "s" });
            if failed > 0 {
                s.push_str(&format!(" · failed {failed}"));
            }
            if unreachable > 0 {
                s.push_str(&format!(" · unreachable {unreachable}"));
            }
            (Some(s), Some(changed > 0))
        }
        "pulumi" => {
            let Some(i) = out.rfind("Resources:") else { return (None, None) };
            let parts: Vec<String> = out[i + "Resources:".len()..]
                .lines()
                .skip(1)
                .map(str::trim)
                .take_while(|l| !l.is_empty())
                .map(String::from)
                .collect();
            if parts.is_empty() {
                return (None, None);
            }
            let changes = parts.iter().any(|p| ["to create", "to update", "to delete", "to replace", "created", "updated", "deleted", "replaced"].iter().any(|w| p.contains(w)));
            (Some(parts.join(" · ")), Some(changes))
        }
        _ => (None, None),
    }
}

/// Replaces secret values in a stream of output, even when a value is split
/// across chunks: the last `longest - 1` bytes are held back until more output
/// (or the end) shows what follows.
pub struct Masker {
    secrets: Vec<String>,
    keep: usize,
    carry: String,
    pending: Vec<u8>,
}

pub const MASK: &str = "••••••";

impl Masker {
    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        let mut secrets: Vec<String> = values.into_iter().filter(|v| v.chars().count() >= 4).collect();
        // Longest first, so a secret containing another is masked whole.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        let keep = secrets.iter().map(|s| s.len()).max().unwrap_or(1) - 1;
        Self { secrets, keep, carry: String::new(), pending: Vec::new() }
    }

    fn mask(&self, s: &str) -> String {
        let mut s = s.to_string();
        for v in &self.secrets {
            if s.contains(v.as_str()) {
                s = s.replace(v.as_str(), MASK);
            }
        }
        s
    }

    /// Bytes in, masked text out (possibly less than came in, until `finish`).
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        // Hold back an incomplete UTF-8 sequence at the end.
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => self.pending.len(),
        };
        let text = String::from_utf8_lossy(&self.pending[..valid]).into_owned();
        self.pending.drain(..valid);
        let joined = std::mem::take(&mut self.carry) + &text;
        let all = self.mask(&joined);
        let mut cut = all.len().saturating_sub(self.keep);
        while !all.is_char_boundary(cut) {
            cut -= 1;
        }
        self.carry = all[cut..].to_string();
        all[..cut].to_string()
    }

    pub fn finish(&mut self) -> String {
        let rest = String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned();
        let joined = std::mem::take(&mut self.carry) + &rest;
        self.mask(&joined)
    }
}

/// `'text'` for sh.
pub fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(p: &[Project]) -> Vec<String> {
        p.iter().map(|p| p.id.clone()).collect()
    }

    #[test]
    fn detects_projects() {
        let files: Vec<String> = [
            "README.md",
            "envs/prod/main.tf",
            "envs/prod/variables.tf",
            "envs/staging/main.tf",
            "modules/vpc/main.tf",
            "site.yml",
            "inventory/hosts.ini",
            "roles/web/tasks/main.yml",
            "group_vars/all.yml",
            "docker-compose.yml",
            "infra/Pulumi.yaml",
            "infra/Pulumi.dev.yaml",
            "infra/Pulumi.prod.yaml",
            "infra/index.ts",
            ".github/workflows/ci.yml",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let read = |f: &str| match f {
            "site.yml" => Some("---\n- name: web\n  hosts: web\n  roles: [web]\n".to_string()),
            _ => Some("services:\n  web:\n    image: x\n".to_string()),
        };
        let p = detect(&files, read);
        assert_eq!(
            names(&p),
            ["ansible:site.yml", "pulumi:infra#dev", "pulumi:infra#prod", "terraform:envs/prod", "terraform:envs/staging"]
        );
        assert_eq!(p[0].dir, "");
        assert_eq!(p[0].file, "site.yml");
        assert_eq!(p[1].name, "infra · dev");
    }

    #[test]
    fn playbooks() {
        assert!(looks_like_playbook("- hosts: all\n  tasks: []\n"));
        assert!(looks_like_playbook("# deploy\n---\n- name: x\n  hosts: localhost\n  connection: local\n"));
        assert!(looks_like_playbook("- import_playbook: web.yml\n"));
        assert!(!looks_like_playbook("- name: a task\n  apt: name=x\n"), "a task list, not plays");
        assert!(!looks_like_playbook("hosts: all\n"), "not a list");
        assert!(!looks_like_playbook("- name: x\n  vars:\n    deep:\n      hosts: y\n"), "nested key");
    }

    #[test]
    fn touched_projects() {
        let p = detect(&["a/main.tf".into(), "b/main.tf".into(), "main.tf".into()], |_| None);
        let t = |changed: &[&str]| -> Vec<String> { touched(&p, &changed.iter().map(|s| s.to_string()).collect::<Vec<_>>()).iter().map(|p| p.id.clone()).collect() };
        assert_eq!(t(&["a/x.tf"]), ["terraform:a"]);
        assert_eq!(t(&["main.tf"]), ["terraform:"]);
        assert_eq!(t(&["README.md", "b/c/d.tf"]), ["terraform:", "terraform:b"]);
        assert_eq!(t(&[]).len(), 3, "unknown = all");
        let mut both = p.clone();
        both.push(Project { id: "ansible:site.yml".into(), kind: "ansible".into(), dir: String::new(), file: "site.yml".into(), stack: String::new(), name: "site.yml".into() });
        let t = |changed: &[&str]| -> Vec<String> { touched(&both, &changed.iter().map(|s| s.to_string()).collect::<Vec<_>>()).iter().map(|p| p.id.clone()).collect() };
        assert_eq!(t(&["a/x.tf"]), ["terraform:a"], "a terraform change doesn't touch the playbook");
        assert_eq!(t(&["roles/web/tasks/main.yml"]), ["ansible:site.yml"]);
    }

    #[test]
    fn summaries() {
        let plan = "…\nPlan: 2 to add, 1 to change, 0 to destroy.\n";
        assert_eq!(summarize("terraform", "plan", plan), (Some("+2 ~1 -0".into()), Some(true)));
        assert_eq!(summarize("terraform", "plan", "No changes. Your infrastructure matches the configuration.\n"), (Some("no changes".into()), Some(false)));
        assert_eq!(
            summarize("terraform", "apply", "Apply complete! Resources: 2 added, 0 changed, 1 destroyed.\n"),
            (Some("2 added · 0 changed · 1 destroyed".into()), Some(true))
        );
        let recap = "PLAY RECAP *****\nweb1 : ok=4 changed=1 unreachable=0 failed=0 skipped=0\nweb2 : ok=3 changed=0 unreachable=0 failed=1 skipped=0\n\n";
        assert_eq!(summarize("ansible", "run", recap), (Some("2 hosts · ok 7 · changed 1 · failed 1".into()), Some(true)));
        let pulumi = "Resources:\n    + 2 to create\n    3 unchanged\n\nDuration: 2s\n";
        assert_eq!(summarize("pulumi", "preview", pulumi), (Some("+ 2 to create · 3 unchanged".into()), Some(true)));
        assert_eq!(summarize("terraform", "plan", "Error: boom"), (None, None));
    }

    #[test]
    fn masks_secrets_across_chunks() {
        let mut m = Masker::new(["hunter2-secret".to_string(), "abc".to_string()]);
        let mut out = String::new();
        for chunk in ["token=hun", "ter2-sec", "ret done ", "ünïcode abc"] {
            out += &m.push(chunk.as_bytes());
        }
        out += &m.finish();
        assert_eq!(out, format!("token={MASK} done ünïcode abc"), "short values are not masked");
        // UTF-8 split across chunks.
        let mut m = Masker::new(Vec::<String>::new());
        let bytes = "é".as_bytes();
        let a = m.push(&bytes[..1]);
        let b = m.push(&bytes[1..]);
        assert_eq!(a + &b + &m.finish(), "é");
    }

    #[test]
    fn run_options() {
        let o = RunOptions { limit: " web ".into(), destroy: true, workspace: "prod".into(), extra_vars: "region=eu\n# c\nreplicas = 3".into(), ..Default::default() };
        let a = o.cleaned("ansible");
        assert_eq!((a.limit.as_str(), a.destroy, a.workspace.as_str()), ("web", false, ""), "terraform fields dropped for ansible");
        assert_eq!(a.extra_json().unwrap().unwrap(), r#"{"region":"eu","replicas":"3"}"#);
        let t = o.cleaned("terraform");
        assert_eq!((t.limit.as_str(), t.destroy, t.extra_vars.as_str()), ("", true, ""));
        assert_eq!(t.describe(), "destroy · workspace prod");
        assert!(RunOptions { workspace: "a b".into(), ..Default::default() }.validate("terraform").is_err());
        assert!(RunOptions { extra_vars: "nope".into(), ..Default::default() }.validate("ansible").is_err());
        assert!(RunOptions { extra_vars: "[1]".into(), ..Default::default() }.validate("ansible").is_err(), "not JSON-object syntax → key=value parse fails");
        assert!(RunOptions { extra_vars: "{\"a\": [1]}".into(), ..Default::default() }.validate("ansible").is_ok());
        assert!(RunOptions { servers: "web; rm".into(), ..Default::default() }.validate("ansible").is_err());
        assert!(RunOptions { verbose: 5, ..Default::default() }.validate("ansible").is_err());
    }

    #[test]
    fn var_names() {
        assert!(valid_var_name("TF_VAR_region"));
        assert!(valid_var_name("_x"));
        assert!(!valid_var_name("1x"));
        assert!(!valid_var_name("A-B"));
        assert!(!valid_var_name(""));
    }
}
