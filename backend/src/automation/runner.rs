//! Executes a run on its runner server:
//!
//! 1. `git archive` of the run's commit (on the timika instance);
//! 2. over SFTP into `~/.timika/runs/<run>/` on the runner (0700), with
//!    `run.env` (the repository's variables, removed as soon as it's read) and
//!    `run.sh` (the tool's commands) — plus the plan file for an apply;
//! 3. `sh run.sh` over SSH; output streams into storage (secrets masked);
//! 4. a plan's `tfplan` comes back into the vault; the run folder is deleted.
//!
//! Terraform projects without a `backend` block keep their state in the vault:
//! `terraform.tfstate` goes to the runner with each run and comes back after
//! every apply (also a failed one — a partial apply changes state too).
//!
//! The run lives on the instance that started it: a heartbeat every few
//! seconds (another instance shows it as `lost` if that stops) and a cancel
//! flag it checks every second, so Cancel works from any instance.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{git, q, Masker, Project, Repo, Run, RunOptions};
use crate::bastion::{self, files, ssh};
use crate::core::Core;
use crate::error::AppError;

const CHUNK_BYTES: usize = 32 * 1024;
const FLUSH_EVERY: Duration = Duration::from_millis(500);
const TAIL_BYTES: usize = 256 * 1024;
const MAX_PLAN_BYTES: usize = 20 * 1024 * 1024;

fn max_log_bytes() -> u64 {
    std::env::var("AUTOMATION_LOG_MAX_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(20u64) * 1024 * 1024
}

fn timeout() -> Duration {
    Duration::from_secs(std::env::var("AUTOMATION_RUN_TIMEOUT_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(60u64) * 60)
}

/// Run output → storage chunks (`automation/logs/<run>/<seq>`), in order.
struct Log {
    core: Arc<Core>,
    id: String,
    seq: u32,
    buf: String,
    bytes: u64,
    max: u64,
    truncated: bool,
    tail: String,
    flushed: Instant,
}

impl Log {
    fn new(core: Arc<Core>, id: &str) -> Self {
        Self { core, id: id.to_string(), seq: 0, buf: String::new(), bytes: 0, max: max_log_bytes(), truncated: false, tail: String::new(), flushed: Instant::now() }
    }

    async fn write(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.tail.push_str(s);
        if self.tail.len() > TAIL_BYTES * 2 {
            let mut cut = self.tail.len() - TAIL_BYTES;
            while !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.tail.drain(..cut);
        }
        if self.truncated {
            return;
        }
        if self.bytes + (self.buf.len() + s.len()) as u64 > self.max {
            self.buf.push_str(&format!("\n… output truncated at {} MB (AUTOMATION_LOG_MAX_MB)\n", self.max / 1024 / 1024));
            self.truncated = true;
            self.flush().await;
            return;
        }
        self.buf.push_str(s);
        if self.buf.len() >= CHUNK_BYTES || self.flushed.elapsed() >= FLUSH_EVERY {
            self.flush().await;
        }
    }

    async fn flush(&mut self) {
        self.flushed = Instant::now();
        if self.buf.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.buf).into_bytes();
        self.bytes += data.len() as u64;
        if let Err(e) = self.core.commit(vec![(super::log_chunk(&self.id, self.seq), data)], vec![], vec![]).await {
            tracing::warn!(run = %self.id, "could not store run output: {e}");
        }
        self.seq += 1;
    }
}

/// The commands for one run (runs in the run folder; `set -e`).
pub fn script(project: &Project, action: &str, tf_bin: &str, o: &RunOptions) -> String {
    let mut s = String::from(
        "set -e\n\
         RUN=$(pwd)\n\
         echo $$ > pid\n\
         if [ -f run.env ]; then set -a; . ./run.env; set +a; rm -f run.env; fi\n\
         mkdir -p src && tar xzf src.tar.gz -C src && rm -f src.tar.gz\n\
         CACHE=\"$RUN/../../cache\"; mkdir -p \"$CACHE\"\n\
         step() { printf '\\n\\342\\226\\266 %s\\n' \"$*\"; }\n\
         need() { command -v \"$1\" >/dev/null 2>&1 || { echo \"$1 is not installed on the runner server\"; exit 127; }; }\n",
    );
    s.push_str(&format!("cd -- {}\n", q(&format!("src/{}", if project.dir.is_empty() { "." } else { &project.dir }))));
    match project.kind.as_str() {
        "terraform" => {
            s.push_str(&format!("TF={}\n", q(tf_bin)));
            if !o.workspace.is_empty() {
                s.push_str(&format!("WS={}\n", q(&o.workspace)));
            }
            s.push_str(
                "if [ \"$TF\" = auto ]; then TF=$(command -v terraform || command -v tofu || echo terraform); fi\n\
                 need \"$TF\"\n\
                 T=$(basename \"$TF\")\n\
                 export TF_IN_AUTOMATION=1 TF_INPUT=0 TF_PLUGIN_CACHE_DIR=\"$CACHE/terraform-plugins\"\n\
                 mkdir -p \"$TF_PLUGIN_CACHE_DIR\"\n\
                 step \"$T init\"\n\
                 STATE=terraform.tfstate\n\
                 if [ -n \"${WS:-}\" ] && [ \"$WS\" != default ]; then STATE=\"terraform.tfstate.d/$WS/terraform.tfstate\"; fi\n\
                 if grep -qsE '^[[:space:]]*(backend[[:space:]]+\"|cloud[[:space:]]*[{])' -- *.tf *.tofu; then\n\
                   echo \"state: the backend configured in the code\"\n\
                 else\n\
                   echo \"state: kept encrypted in timika (no backend configured)\"\n\
                   if [ -f \"$RUN/tfstate\" ]; then mkdir -p \"$(dirname \"$STATE\")\"; cp \"$RUN/tfstate\" \"$STATE\"; fi\n\
                 fi\n\
                 \"$TF\" init -input=false -no-color\n",
            );
            if !o.workspace.is_empty() {
                s.push_str("step \"$T workspace select -or-create $WS\"\n\"$TF\" workspace select -or-create \"$WS\"\n");
            }
            if action == "apply" {
                s.push_str("cp \"$RUN/tfplan\" tfplan\nstep \"$T apply (the reviewed plan)\"\n\"$TF\" apply -input=false -no-color -auto-approve tfplan\n");
            } else {
                let mut args = String::new();
                if o.destroy {
                    args.push_str(" -destroy");
                }
                for t in &o.targets {
                    args.push_str(&format!(" -target={}", q(t)));
                }
                for t in &o.replace {
                    args.push_str(&format!(" -replace={}", q(t)));
                }
                let shown = o.describe();
                s.push_str(&format!(
                    "step \"$T plan{}\"\n\"$TF\" plan -input=false -no-color -out=tfplan{args}\n",
                    if shown.is_empty() { String::new() } else { format!(" ({})", shown.replace(['"', '$', '`', '\\'], "")) }
                ));
            }
        }
        "ansible" => {
            let mut check = String::from(if action == "check" { " --check --diff" } else { "" });
            for (flag, v) in [("--limit", &o.limit), ("--tags", &o.tags), ("--skip-tags", &o.skip_tags)] {
                if !v.is_empty() {
                    check.push_str(&format!(" {flag} {}", q(v)));
                }
            }
            if !o.extra_vars.trim().is_empty() {
                check.push_str(" -e @\"$RUN/extra.json\"");
            }
            if o.verbose > 0 {
                check.push_str(&format!(" -{}", "v".repeat(o.verbose.min(4) as usize)));
            }
            let check = check.as_str();
            let inv = if o.servers.is_empty() {
                "for f in inventory inventory.ini inventory.yml inventory.yaml hosts hosts.ini hosts.yml; do if [ -e \"$f\" ]; then INV=\"$f\"; break; fi; done\n".to_string()
            } else {
                // timika's servers: host keys must match the fingerprints timika pinned.
                "INV=\"$RUN/timika-inventory.yml\"\nexport TIMIKA_RUN_DIR=\"$RUN\"\nchmod 600 \"$RUN\"/keys/* \"$INV\" 2>/dev/null || true\n\
                 need ssh-keyscan\n\
                 step \"host keys (checked against timika's pinned fingerprints)\"\n\
                 : > \"$RUN/known_hosts\"\n\
                 while read -r H P FP NAME; do\n\
                   if [ \"$FP\" = - ]; then echo \"  ✗ $NAME: no pinned host key yet — connect to it once in timika\"; continue; fi\n\
                   ssh-keyscan -T 5 -p \"$P\" \"$H\" 2>/dev/null > \"$RUN/scan\" || true\n\
                   OK=\n\
                   while read -r L; do\n\
                     F=$(printf '%s\\n' \"$L\" | ssh-keygen -lf - 2>/dev/null | awk '{print $2}')\n\
                     if [ \"$F\" = \"$FP\" ]; then printf '%s\\n' \"$L\" >> \"$RUN/known_hosts\"; OK=1; fi\n\
                   done < \"$RUN/scan\"\n\
                   if [ -n \"$OK\" ]; then echo \"  ✓ $NAME\"; else echo \"  ✗ $NAME: the host key doesn't match timika's pin — refused\"; fi\n\
                 done < \"$RUN/hosts.txt\"\n".to_string()
            };
            s.push_str(&format!(
                "need ansible-playbook\n\
                 export ANSIBLE_NOCOLOR=1 ANSIBLE_FORCE_COLOR=0 ANSIBLE_RETRY_FILES_ENABLED=0 PYTHONUNBUFFERED=1\n\
                 INV=\n\
                 {inv}\
                 if [ -f requirements.yml ]; then step \"ansible-galaxy install -r requirements.yml\"; ansible-galaxy install -r requirements.yml; fi\n\
                 step \"ansible-playbook {file}${{INV:+ -i $INV}}{shown}\"\n\
                 if [ -n \"$INV\" ]; then ansible-playbook -i \"$INV\" {qf}{check}; else ansible-playbook {qf}{check}; fi\n",
                file = project.file.replace(['"', '$', '`', '\\'], ""),
                shown = check.replace(['"', '$', '`', '\\'], "").replace(" -e @RUN/extra.json", " -e @extra.json"),
                qf = q(&project.file),
            ));
        }
        "pulumi" => {
            let mut stack = if project.stack.is_empty() { String::new() } else { format!(" --stack {}", q(&project.stack)) };
            for t in &o.targets {
                stack.push_str(&format!(" --target {}", q(t)));
            }
            if o.refresh {
                stack.push_str(" --refresh");
            }
            s.push_str("need pulumi\nexport PULUMI_SKIP_UPDATE_CHECK=1\n");
            s.push_str(
                "if [ -z \"${PULUMI_BACKEND_URL:-}\" ] && [ -z \"${PULUMI_ACCESS_TOKEN:-}\" ]; then\n\
                   mkdir -p \"$CACHE/pulumi\"; export PULUMI_BACKEND_URL=\"file://$(cd \"$CACHE/pulumi\" && pwd)\"\n\
                   export PULUMI_CONFIG_PASSPHRASE=\"${PULUMI_CONFIG_PASSPHRASE:-}\"\n\
                   echo \"state: on the runner (~/.timika/cache/pulumi) — set PULUMI_BACKEND_URL or PULUMI_ACCESS_TOKEN to use your own\"\n\
                 fi\n",
            );
            s.push_str("if pulumi install --help >/dev/null 2>&1; then step \"pulumi install\"; pulumi install; fi\n");
            if !project.stack.is_empty() {
                s.push_str(&format!("pulumi stack select --create --non-interactive {}\n", q(&project.stack)));
            }
            if action == "up" {
                s.push_str(&format!("step \"pulumi up\"\npulumi up --yes --skip-preview --non-interactive --color never{stack}\n"));
            } else {
                s.push_str(&format!("step \"pulumi preview\"\npulumi preview --non-interactive --color never{stack}\n"));
            }
        }
        _ => s.push_str("echo 'unknown project kind'; exit 2\n"),
    }
    s
}

/// `run.env`: the repository's variables (secret values included).
pub fn env_file(vars: &[(String, String)]) -> String {
    vars.iter().map(|(k, v)| format!("{k}={}\n", q(v))).collect()
}

/// Ends a run that couldn't start (or crashed) as failed, with the reason in its output.
async fn fail(core: &Arc<Core>, id: &str, log: &mut Log, msg: &str) {
    log.write(&format!("\n✗ {msg}\n")).await;
    log.flush().await;
    let (bytes, truncated) = (log.bytes, log.truncated);
    let _ = super::update_run(core, id, |r| {
        r.status = "failed".into();
        r.error = Some(msg.to_string());
        r.finished_at = Some(Utc::now());
        r.log_bytes = bytes;
        r.truncated = truncated;
    })
    .await;
}

/// Execute a queued run to the end. Spawned by StartRun / the webhook.
pub async fn execute(core: Arc<Core>, id: String) {
    let mut log = Log::new(core.clone(), &id);
    if let Err(msg) = execute_inner(&core, &id, &mut log).await {
        fail(&core, &id, &mut log, &msg).await;
    }
    // Tell whoever wants to know.
    if let Ok(run) = super::get_run(&core, &id).await {
        let safe = !super::changes_things(&run.kind, &run.action);
        let event = match run.status.as_str() {
            "failed" => Some("failed"),
            "succeeded" if run.trigger == "schedule" && safe && run.changes == Some(true) => Some("drift"),
            "succeeded" => Some("succeeded"),
            _ => None,
        };
        if let Some(e) = event {
            super::notify::run_event(&core, &run, e).await;
        }
    }
}

/// An Ansible inventory of timika's servers with these tags (`*` = all):
/// one host per server, its first account, credentials as key files /
/// passwords. Also `hosts.txt` (host port pinned-fingerprint name) for the
/// host-key check, and the passwords (to mask).
pub struct ServerInventory {
    pub yaml: String,
    pub hosts: String,
    pub keys: Vec<(String, String)>,
    pub passwords: Vec<String>,
    pub count: usize,
}

pub async fn server_inventory(core: &Core, selector: &str) -> Result<ServerInventory, String> {
    use serde_json::{json, Map, Value};
    let want: Vec<String> = selector.split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect();
    let all = want.iter().any(|t| t == "*");
    let group = |t: &str| -> String { t.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect() };
    let mut hosts = Map::new();
    let mut children: Map<String, Value> = Map::new();
    let mut inv = ServerInventory { yaml: String::new(), hosts: String::new(), keys: vec![], passwords: vec![], count: 0 };
    for a in bastion::list_assets(core).await.map_err(err)? {
        let tags: Vec<String> = a.tags.iter().map(|t| t.to_lowercase()).collect();
        if !all && !want.iter().any(|w| tags.contains(w)) {
            continue;
        }
        let Some(acc) = a.accounts.first() else { continue };
        let secret: bastion::Secret = core.get_json(&bastion::cred_path(&a.id, &acc.username)).await.map_err(err)?.unwrap_or_default();
        let name = bastion::slug(&a.name);
        let mut h = Map::new();
        h.insert("ansible_host".into(), json!(a.host));
        h.insert("ansible_port".into(), json!(a.port));
        h.insert("ansible_user".into(), json!(acc.username));
        if let Some(k) = secret.private_key.filter(|k| !k.trim().is_empty()) {
            if secret.passphrase.as_deref().is_some_and(|p| !p.is_empty()) {
                // ssh can't take a passphrase non-interactively: use it without one.
                let Ok(key) = russh::keys::decode_secret_key(&k, secret.passphrase.as_deref()) else { continue };
                let Ok(pem) = key.to_openssh(russh::keys::ssh_key::LineEnding::LF) else { continue };
                inv.keys.push((name.clone(), pem.to_string()));
            } else {
                inv.keys.push((name.clone(), k));
            }
            h.insert("ansible_ssh_private_key_file".into(), json!(format!("{{{{ lookup('env', 'TIMIKA_RUN_DIR') }}}}/keys/{name}")));
        } else if let Some(p) = secret.password.filter(|p| !p.is_empty()) {
            inv.passwords.push(p.clone());
            h.insert("ansible_password".into(), json!(p));
        }
        hosts.insert(name.clone(), Value::Object(h));
        for t in &a.tags {
            let g = children.entry(group(t)).or_insert_with(|| json!({ "hosts": {} }));
            g["hosts"][&name] = json!({});
        }
        inv.hosts.push_str(&format!("{} {} {} {name}\n", a.host, a.port, a.host_key.as_deref().unwrap_or("-")));
        inv.count += 1;
    }
    if inv.count == 0 {
        return Err(format!("no servers in timika match `{selector}`"));
    }
    let doc = json!({ "all": {
        "vars": { "ansible_ssh_common_args": "-o StrictHostKeyChecking=yes -o UserKnownHostsFile={{ lookup('env', 'TIMIKA_RUN_DIR') }}/known_hosts" },
        "hosts": hosts,
        "children": children,
    }});
    // JSON is valid YAML.
    inv.yaml = serde_json::to_string_pretty(&doc).unwrap_or_default();
    Ok(inv)
}

fn err(e: AppError) -> String {
    match e {
        AppError::BadRequest(m) | AppError::NotFound(m) | AppError::Forbidden(m) | AppError::Unavailable(m) | AppError::Conflict(m) | AppError::Auth(m) => m,
        other => other.to_string(),
    }
}

async fn execute_inner(core: &Arc<Core>, id: &str, log: &mut Log) -> Result<(), String> {
    let run = super::get_run(core, id).await.map_err(err)?;
    let repo: Repo = super::get_repo(core, &run.repo).await.map_err(err)?;
    let secrets = super::get_secrets(core, &run.repo).await.map_err(err)?;
    let project = repo.projects.iter().find(|p| p.id == run.project).cloned().ok_or("this project is no longer in the repository — pull again")?;

    let instance = core.identity.id.clone();
    super::update_run(core, id, |r| {
        r.status = "running".into();
        r.started_at = Some(Utc::now());
        r.heartbeat_at = Utc::now();
        r.instance = instance.clone();
    })
    .await
    .map_err(err)?;
    let short = &run.sha[..run.sha.len().min(7)];
    log.write(&format!("▶ {} {} · {} · commit {short} · runner {}\n", project.kind, run.action, project.name, run.runner_name)).await;

    // 1. The code at this commit.
    let auth = git::Auth { token: secrets.token.clone(), private_key: secrets.private_key.clone() };
    let snapshot = git::archive(&repo.id, &repo.url, &repo.branch, &auth, &run.sha).await?;

    // Variables: plain ones, secrets from the vault, and a few about the run.
    let mut vars: Vec<(String, String)> = Vec::new();
    for v in &repo.variables {
        let value = if v.secret { secrets.vars.get(&v.name).cloned() } else { v.value.clone() };
        if let Some(value) = value {
            vars.push((v.name.clone(), value));
        }
    }
    vars.push(("TIMIKA_RUN".into(), id.to_string()));
    vars.push(("TIMIKA_REPO".into(), repo.name.clone()));
    vars.push(("TIMIKA_COMMIT".into(), run.sha.clone()));
    let inventory = if project.kind == "ansible" && !run.options.servers.is_empty() { Some(server_inventory(core, &run.options.servers).await?) } else { None };
    let secret_values: Vec<String> = repo
        .variables
        .iter()
        .filter(|v| v.secret)
        .filter_map(|v| secrets.vars.get(&v.name).cloned())
        .chain(secrets.token.clone())
        .chain(inventory.iter().flat_map(|i| i.passwords.clone()))
        .collect();
    let mut masker = Masker::new(secret_values);
    let extra = run.options.extra_json()?;

    let plan = match &run.plan_run {
        Some(p) => Some(core.get(&super::plan_path(p)).await.map_err(err)?.ok_or("the plan file is gone — plan again")?),
        None => None,
    };

    // 2. Onto the runner.
    let asset = bastion::get_asset(core, &run.runner_asset).await.map_err(err)?;
    let conn = files::open(core, &asset, &run.runner_account).await.map_err(err)?;
    let dir = format!(".timika/runs/{id}");
    for d in [".timika", ".timika/runs", &dir] {
        if !conn.sftp.try_exists(d.to_string()).await.unwrap_or(false) {
            conn.sftp.create_dir(d.to_string()).await.map_err(|e| format!("can't create {d} on the runner: {e}"))?;
        }
    }
    let private = russh_sftp::protocol::FileAttributes { permissions: Some(0o700), ..Default::default() };
    let _ = conn.sftp.set_metadata(".timika".to_string(), private).await;
    let put = |name: String, data: Vec<u8>| {
        let sftp = &conn.sftp;
        let path = format!("{dir}/{name}");
        async move {
            let mut f = sftp.create(path.clone()).await.map_err(|e| format!("can't write {path} on the runner: {e}"))?;
            f.write_all(&data).await.map_err(|e| format!("can't write {path} on the runner: {e}"))?;
            f.shutdown().await.map_err(|e| format!("can't write {path} on the runner: {e}"))?;
            Ok::<_, String>(())
        }
    };
    put("src.tar.gz".into(), snapshot).await?;
    put("run.env".into(), env_file(&vars).into_bytes()).await?;
    put("run.sh".into(), script(&project, &run.action, &repo.tf_bin, &run.options).into_bytes()).await?;
    if let Some(plan) = plan {
        put("tfplan".into(), plan).await?;
    }
    if let Some(x) = extra {
        put("extra.json".into(), x.into_bytes()).await?;
    }
    if let Some(inv) = &inventory {
        log.write(&format!("hosts: {} server{} from timika ({})\n", inv.count, if inv.count == 1 { "" } else { "s" }, run.options.servers)).await;
        put("timika-inventory.yml".into(), inv.yaml.clone().into_bytes()).await?;
        put("hosts.txt".into(), inv.hosts.clone().into_bytes()).await?;
        if !inv.keys.is_empty() {
            let _ = conn.sftp.create_dir(format!("{dir}/keys")).await;
            for (name, key) in &inv.keys {
                put(format!("keys/{name}"), key.clone().into_bytes()).await?;
            }
        }
    }
    let workspace = if run.options.workspace.is_empty() || run.options.workspace == "default" { None } else { Some(run.options.workspace.clone()) };
    let state_id = match &workspace {
        Some(w) => format!("{}@{w}", project.id),
        None => project.id.clone(),
    };
    let state_key = super::state_path(&repo.id, &state_id);
    let old_state = if project.kind == "terraform" { core.get(&state_key).await.map_err(err)? } else { None };
    if let Some(s) = &old_state {
        put("tfstate".into(), s.clone()).await?;
    }

    // 3. Run it.
    let cd = format!("cd -- {}", q(&dir));
    let mut ch = ssh::exec_stream(&conn.handle, &format!("{cd} && sh run.sh 2>&1")).await?;
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut code: Option<u32> = None;
    let mut stopped: Option<&str> = None;
    let mut beat = Instant::now();
    let kill = format!("{cd} && P=$(cat pid 2>/dev/null) && {{ pkill -TERM -P \"$P\" 2>/dev/null; kill -TERM \"$P\" 2>/dev/null; }}; true");
    let mut grace: Option<Instant> = None;
    loop {
        tokio::select! {
            msg = ch.wait() => match msg {
                Some(russh::ChannelMsg::Data { data }) | Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                    let text = masker.push(&data);
                    log.write(&text).await;
                }
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
                Some(russh::ChannelMsg::Close) | None => break,
                _ => {}
            },
            _ = tick.tick() => {
                if log.flushed.elapsed() >= FLUSH_EVERY {
                    log.flush().await;
                }
                if grace.is_some_and(|g| g.elapsed() > Duration::from_secs(15)) {
                    break;
                }
                if stopped.is_none() {
                    let cancel = super::get_run(core, id).await.map(|r| r.cancel).unwrap_or(false);
                    let over = started.elapsed() > timeout();
                    if cancel || over {
                        stopped = Some(if cancel { "cancelled" } else { "timeout" });
                        log.write(&format!("\n■ {}\n", if cancel { "cancelled — stopping…".to_string() } else { format!("time limit reached ({} min) — stopping…", timeout().as_secs() / 60) })).await;
                        let _ = ssh::exec(&conn.handle, &kill, b"").await;
                        grace = Some(Instant::now());
                    }
                }
                if beat.elapsed() > Duration::from_secs(5) {
                    beat = Instant::now();
                    let _ = super::update_run(core, id, |r| r.heartbeat_at = Utc::now()).await;
                }
            }
        }
    }
    log.write(&masker.finish()).await;

    // 4. Keep the plan; clean up.
    let ok = code == Some(0) && stopped.is_none();
    let mut plan_err = None;
    if ok && project.kind == "terraform" && run.action == "plan" {
        let path = format!("{dir}/src/{}/tfplan", if project.dir.is_empty() { "." } else { &project.dir });
        match conn.sftp.open(path).await {
            Ok(mut f) => {
                let mut data = Vec::new();
                let read = (&mut f).take(MAX_PLAN_BYTES as u64 + 1).read_to_end(&mut data).await;
                if read.is_err() || data.len() > MAX_PLAN_BYTES {
                    plan_err = Some("the plan file could not be kept (over 20 MB?) — Apply is not available for this plan".to_string());
                } else if let Err(e) = core.commit(vec![(super::plan_path(id), data)], vec![], vec![]).await {
                    plan_err = Some(format!("the plan file could not be stored: {e}"));
                }
            }
            Err(e) => plan_err = Some(format!("the plan file could not be read back: {e}")),
        }
    }
    if project.kind == "terraform" && run.action == "apply" {
        let file = match &workspace {
            Some(w) => format!("terraform.tfstate.d/{w}/terraform.tfstate"),
            None => "terraform.tfstate".into(),
        };
        let path = format!("{dir}/src/{}/{file}", if project.dir.is_empty() { "." } else { &project.dir });
        if let Ok(new) = conn.sftp.read(path).await {
            if !new.is_empty() && old_state.as_ref() != Some(&new) {
                let mut puts = vec![(state_key.clone(), new)];
                if let Some(old) = old_state.clone() {
                    puts.push((super::prev_state_path(&repo.id, &state_id), old));
                }
                match core.commit(puts, vec![], vec![]).await {
                    Ok(()) => log.write("\nstate saved (encrypted) in timika\n").await,
                    Err(e) => plan_err = Some(format!("THE NEW STATE COULD NOT BE SAVED: {e} — it is on the runner in {dir}; copy it before the next run")),
                }
            }
        }
    }
    // Keep the folder if its state couldn't be saved — it's the only copy.
    if plan_err.as_deref().is_none_or(|e| !e.starts_with("THE NEW STATE")) {
        let _ = ssh::exec(&conn.handle, &format!("cd -- '.timika/runs' && rm -rf -- {}", q(id)), b"").await;
    }
    let _ = conn.sftp.close().await;
    ssh::disconnect(&conn.handle).await;

    if let Some(e) = &plan_err {
        log.write(&format!("\n! {e}\n")).await;
    }
    let (summary, changes) = super::summarize(&project.kind, &run.action, &log.tail);
    let status = match stopped {
        Some("cancelled") => "cancelled",
        Some(_) => "failed",
        None if ok => "succeeded",
        None => "failed",
    };
    let error = match (stopped, code) {
        (Some("timeout"), _) => Some(format!("stopped after the {} min time limit", timeout().as_secs() / 60)),
        (Some(_), _) => None,
        (None, Some(0)) => None,
        (None, Some(127)) => Some("a tool is missing on the runner server".into()),
        (None, Some(c)) => Some(format!("exited with code {c}")),
        (None, None) => Some("the connection to the runner closed unexpectedly".into()),
    };
    let finished = format!("\n{} {status} in {}s\n", if status == "succeeded" { "✓" } else { "✗" }, started.elapsed().as_secs());
    log.write(&finished).await;
    log.flush().await;
    let (bytes, truncated) = (log.bytes, log.truncated);
    super::update_run(core, id, |r| {
        r.status = status.into();
        r.finished_at = Some(Utc::now());
        r.exit_code = code.map(|c| c as i32);
        r.summary = summary.clone();
        r.changes = changes;
        r.error = error.clone();
        r.log_bytes = bytes;
        r.truncated = truncated;
    })
    .await
    .map_err(err)?;
    Ok(())
}

/// The tools on a runner server.
pub async fn check(core: &Core, asset: &bastion::Asset, account: &str) -> Result<Vec<(String, String)>, String> {
    let conn = files::open(core, asset, account).await.map_err(err)?;
    let cmd = "cd -- '.' && for t in terraform tofu ansible-playbook pulumi; do p=$(command -v $t 2>/dev/null) || continue; \
               v=$($t --version 2>/dev/null | head -n 1); echo \"$t|$v\"; done; true";
    let r = ssh::exec(&conn.handle, cmd, b"").await;
    let _ = conn.sftp.close().await;
    ssh::disconnect(&conn.handle).await;
    let (_, out) = r?;
    Ok(out
        .lines()
        .filter_map(|l| l.split_once('|'))
        .map(|(t, v)| (t.to_string(), v.trim().trim_start_matches("Terraform ").trim_start_matches("OpenTofu ").trim_start_matches("ansible-playbook ").trim_start_matches("[core ").trim_end_matches(']').to_string()))
        .collect())
}

/// A queued run record (StartRun and the webhook create these).
#[allow(clippy::too_many_arguments)]
pub fn new_run(repo: &Repo, project: &Project, action: &str, user: &str, trigger: &str, sha: &str, message: &str, plan_run: Option<String>) -> Run {
    let now = Utc::now();
    Run {
        id: bastion::random_id(8),
        repo: repo.id.clone(),
        repo_name: repo.name.clone(),
        project: project.id.clone(),
        project_name: project.name.clone(),
        kind: project.kind.clone(),
        action: action.into(),
        status: "queued".into(),
        user: user.into(),
        trigger: trigger.into(),
        sha: sha.into(),
        commit_message: message.into(),
        runner_asset: repo.runner_asset.clone(),
        runner_account: repo.runner_account.clone(),
        runner_name: String::new(),
        instance: String::new(),
        created_at: now,
        started_at: None,
        finished_at: None,
        heartbeat_at: now,
        exit_code: None,
        summary: None,
        changes: None,
        error: None,
        plan_run,
        applied_by: None,
        cancel: false,
        options: RunOptions::default(),
        approved_by: None,
        log_chunks: 0,
        log_bytes: 0,
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(kind: &str, dir: &str, file: &str, stack: &str) -> Project {
        Project { id: String::new(), kind: kind.into(), dir: dir.into(), file: file.into(), stack: stack.into(), name: String::new() }
    }

    #[test]
    fn scripts() {
        let s = script(&p("terraform", "envs/prod", "", ""), "plan", "auto", &RunOptions::default());
        assert!(s.contains("cd -- 'src/envs/prod'"));
        assert!(s.contains("plan -input=false -no-color -out=tfplan"));
        let s = script(&p("terraform", "", "", ""), "apply", "tofu", &RunOptions::default());
        assert!(s.contains("cd -- 'src/.'") && s.contains("TF='tofu'") && s.contains("apply -input=false -no-color -auto-approve tfplan"));
        let s = script(&p("ansible", "play", "it's.yml", ""), "check", "auto", &RunOptions::default());
        assert!(s.contains(r"ansible-playbook 'it'\''s.yml' --check --diff"), "{s}");
        let s = script(&p("pulumi", "infra", "", "prod"), "up", "auto", &RunOptions::default());
        assert!(s.contains("pulumi up --yes --skip-preview --non-interactive --color never --stack 'prod'"));
    }

    #[test]
    fn scripts_with_options() {
        let o = RunOptions { destroy: true, targets: vec!["aws_instance.web".into()], replace: vec!["x.y".into()], workspace: "staging".into(), ..Default::default() };
        let s = script(&p("terraform", "infra", "", ""), "plan", "auto", &o);
        assert!(s.contains("WS='staging'") && s.contains("workspace select -or-create \"$WS\""), "{s}");
        assert!(s.contains("plan -input=false -no-color -out=tfplan -destroy -target='aws_instance.web' -replace='x.y'"), "{s}");
        let o = RunOptions { limit: "web*".into(), tags: "deploy".into(), extra_vars: "a=1".into(), verbose: 2, servers: "prod".into(), ..Default::default() };
        let s = script(&p("ansible", "", "site.yml", ""), "run", "auto", &o);
        assert!(s.contains("ansible-playbook -i \"$INV\" 'site.yml' --limit 'web*' --tags 'deploy' -e @\"$RUN/extra.json\" -vv"), "{s}");
        assert!(s.contains("timika-inventory.yml") && s.contains("ssh-keyscan"), "servers inventory");
        let o = RunOptions { limit: "x'; rm -rf / #".into(), ..Default::default() };
        assert!(script(&p("ansible", "", "site.yml", ""), "run", "auto", &o).contains(r"--limit 'x'\''; rm -rf / #'"), "quoted");
    }

    #[test]
    fn env_quoting() {
        assert_eq!(env_file(&[("A".into(), "x'y $HOME".into())]), "A='x'\\''y $HOME'\n");
    }
}
