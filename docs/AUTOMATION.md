# Infrastructure — Terraform, OpenTofu, Ansible, Pulumi from Git

A bastion gets people *into* servers. Infrastructure code changes them on a
schedule people can review. **Infrastructure** (sidebar → Automation) connects a
Git repository and runs the code in it:

| Found in the repository | Actions |
|---|---|
| **Terraform / OpenTofu**: every folder with `*.tf` / `*.tofu` (not `modules/`) | **Plan** → **Apply** (exactly that plan) |
| **Ansible**: every playbook (YAML that is a list of plays with `hosts:`), not `roles/`, `group_vars/`… | **Check** (`--check --diff`) · **Run** |
| **Pulumi**: `Pulumi.yaml`, one project per `Pulumi.<stack>.yaml` | **Preview** · **Up** |

Runs execute on a **runner server**: one of the servers already in timika,
over SSH with its stored credentials and pinned host key. **Code from a
repository never runs on the vault host.**

On top of one-click runs (ideas taken from [Semaphore UI](https://semaphoreui.com)):

| | |
|---|---|
| [Run options](#run-options) | Ansible limit / tags / extra vars / verbosity; Terraform workspace, destroy, `-target`, `-replace`; Pulumi targets, refresh |
| [Ansible against timika's servers](#ansible-against-timikas-servers) | Hosts by tag from the bastion, credentials from the vault, host keys checked against timika's pins: no inventory file, no keys to copy |
| [Approvals](#approvals) | Apply / Run / Up wait for a second admin |
| [Schedules](#schedules) | Cron: a nightly plan reports drift, a weekly playbook keeps servers patched |
| [Notifications](#notifications) | Slack, Teams, Discord, Telegram, any webhook: on failure, drift, approval needed, success |
| [CI trigger](#ci-trigger) | `POST` with a token from GitHub Actions / GitLab CI; optionally wait, so the CI step fails if the run does |

---

## Connect a repository (one form)

*Infrastructure → Connect a repository*:

1. **Paste the URL**: `https://github.com/acme/infra` or `git@github.com:acme/infra.git`.
2. **Access**:
   - **Public**: nothing to do.
   - **Token**: a read-only token (GitHub fine-grained: *Contents → Read-only*;
     GitLab: `read_repository`). Stored in the vault and sent only in an HTTP
     header, never on a command line or in a URL.
   - **Deploy key**: chosen automatically for `git@…` URLs. timika generates an
     Ed25519 key, shows the public half and links to the host's *Deploy keys*
     page. Add it (read-only), then press Connect. The private half never leaves
     the vault.
3. **Runner server**: `account@server` from your servers. **Check** lists the
   tools it has (terraform, tofu, ansible-playbook, pulumi).
4. **Connect**. timika clones the repository right away, so wrong access fails
   here and not at the first run. It also finds the default branch and the
   projects.

Optional: **Variables & secrets** (see below), the branch, a name, `terraform`
vs `tofu`, and **plan on push** (on by default).

## Run

*Repository → Projects*. Each project shows its last run and its buttons:

- **Plan** (Terraform) shows what would change: `+2 ~1 -0`. **Apply +2 ~1 -0**
  then applies **that plan file**, at that plan's commit, once. A second Apply
  of the same plan is refused. Plan again to change anything.
- **Check / Run** (Ansible) and **Preview / Up** (Pulumi). Run and Up ask for
  confirmation.
- Only one run per project at a time.

Each run opens its page: **live output** (plan lines coloured, steps marked
`▶`), the summary, who started it, from which commit, on which runner, and how
long it took. The page also has **Cancel**, **Apply this plan**, **Run again**,
and **Output** (download the log).

### Look at the code

*Repository → Files* (or the **</>** button on a project) browses the repository
at the pulled commit: folders first, breadcrumbs, and a viewer with line numbers,
light colouring (comments, strings, keywords), **Wrap**, **Copy** and
**Download**. Binary files and files over 1 MB show only their size.

It reads timika's own clone, so nothing is sent to the runner, and what you see
is what a run at that commit would use. Only admins can browse: committed code
sometimes holds secrets. Each read is in the audit trail by path, never content.

### Run options

The **⚙** next to a project's buttons opens *Run with options…*. One click on
Plan / Check / Preview still runs without any.

| Kind | Options |
|---|---|
| Terraform / OpenTofu | **Workspace** (created if missing; each has its own state), **Destroy plan** (`plan -destroy`, then Apply that plan as usual), **targets** (`-target`), **replace** (`-replace`) |
| Ansible | **Hosts**: the repository's inventory or timika's servers (below), **limit**, **tags**, **skip tags**, **extra vars** (`key=value` lines or JSON, passed as `-e @extra.json`), **verbosity** `-v`…`-vvvv` |
| Pulumi | **targets** (`--target`), **refresh** (`--refresh`) |

Apply always uses the options of the plan it applies (its workspace). Every
value is passed as one quoted argument, never pasted into a shell. The run page
and the lists show what was used ("destroy · workspace staging").

### Ansible against timika's servers

*Hosts → Servers in timika* and pick tags (or *all servers*):

- One inventory host per server: its first account, with the stored private
  key (a file, 0600) or password (`ansible_password`, which needs `sshpass` on
  the runner). Inventory groups are the tags, so `--limit web` works.
- **Host keys:** before the playbook, the runner scans each server and keeps
  only keys whose fingerprint matches the one timika pinned
  (`StrictHostKeyChecking=yes` against that file). A server never connected to
  through timika (not pinned) or with a different key is refused. The output
  shows ✓ / ✗ per server.
- Passwords are masked in the output. Keys and the inventory exist only in the
  run folder, which is deleted afterwards.

### What happens on the runner

```
~/.timika/runs/<run>/   (0700, deleted afterwards)
  src.tar.gz   git archive of the run's commit — committed files only
  run.env      the repository's variables — deleted as soon as it's read
  run.sh       the tool's commands
  tfplan       (apply) the plan being applied
  tfstate      (Terraform, no backend) the project's state
~/.timika/cache/       provider plugin cache, Pulumi file state
```

- Terraform: `init` and `plan -out=tfplan` / `apply tfplan`, with
  `TF_IN_AUTOMATION=1` and a shared plugin cache.
- Ansible: `ansible-playbook` with the project's `inventory` / `hosts` file if
  there is one, and `requirements.yml` installed first.
- Pulumi: `pulumi install`, `stack select --create`, `preview` / `up --yes`.

Every run also gets `TIMIKA_RUN`, `TIMIKA_REPO` and `TIMIKA_COMMIT`.

### Terraform state

- **A backend in the code** (`backend "s3"`, `cloud {}`…): used as is.
- **No backend:** timika keeps `terraform.tfstate` **encrypted in the vault**.
  It goes to the runner with each run and comes back after every apply, even
  a failed one, because a partial apply changes state too. The previous
  version is kept as a backup. If the new state can't be stored, the run folder
  is kept on the runner (the output says where).

Pulumi without `PULUMI_BACKEND_URL` / `PULUMI_ACCESS_TOKEN` uses a file
backend in the runner's `~/.timika/cache/pulumi`.

## Variables & secrets

Environment variables for every run: `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, `ARM_CLIENT_SECRET`, `GOOGLE_CREDENTIALS`,
`TF_VAR_region`, `PULUMI_ACCESS_TOKEN`…

- Mark a variable **secret** (the lock; names like `*SECRET*`, `*TOKEN*` and
  `*PASSWORD*` are marked automatically). Its value is stored in the vault and
  **never returned**: leave it empty when editing to keep it.
- Secret values are **masked** (`••••••`) in all output, even when split
  across chunks of output.
- On the runner the values exist only in the process environment; `run.env` is
  deleted as soon as the run starts.

## Approvals

*Setup → Approvals*: **Apply, Run and Up wait until another admin approves.**

- The run is created as *awaiting approval* and nothing executes. The project
  shows *Review approval*. Notifications with *Needs approval* are sent.
- The approver sees what the run will do: for an Apply, the plan's summary and a
  link to its full output. **Approve & run** starts it; **Reject** cancels it.
  The person who asked can withdraw it but not approve it.
- A rejected Apply frees its plan, so it can be applied (and approved) later.
- Plans, checks and previews never wait. CI and schedules follow the same rule.

## Schedules

*Repository → Schedules → New schedule*: a project, an action (not Apply),
when, and a time zone.

- **When**: every hour / day / weekday / Monday / 1st of the month at a time, or
  any **cron** expression (5 fields, `*/15`, `1-5`, `mon-fri`, `@daily`…).
- **Time zone**: `UTC` or a fixed offset (`+07:00`; the browser's is filled in).
  Fixed offsets don't follow daylight-saving changes.
- **Drift**: a scheduled Plan / Check / Preview that finds changes sends a
  *Drift found* notification. Review the plan, then Apply it (it isn't applied
  automatically).
- Each due time fires **once**, even with several timika instances: the time is
  claimed with a guarded write. With Raft only the leader schedules. A time
  missed while timika was down or sealed for more than 10 minutes is skipped and
  noted, not replayed.
- Pause/resume, **Run now**, and edit. A changed schedule counts from now.

## Notifications

*Setup → Notifications → Add a notification*:

| Type | Target |
|---|---|
| Slack, Microsoft Teams (incoming webhook), Mattermost, Rocket.Chat | webhook URL; sends `{"text"}` |
| Discord | channel webhook URL |
| Telegram | bot token (@BotFather) + chat id |
| Webhook | any URL; gets `{event, text, run: {id, repo, project, action, status, summary, error, user, trigger, commit, options, url}}` |

Choose the events: **Failed**, **Drift found**, **Needs approval**,
**Succeeded**. **Send a test** before adding. The URL / token is stored with
the repository's secrets, and only a redacted label is shown afterwards. Set
`PUBLIC_URL` so messages link to the run.

## CI trigger

*Setup → CI trigger* shows the URL and token (`tmkci.…`, admins only;
**Rotate** invalidates the old one) and a ready-to-paste GitHub Actions step:

```bash
curl --fail -sS -X POST "https://timika.example.com/v1/automation/trigger/<repo>" \
  -H "Authorization: Bearer $TIMIKA_TOKEN" \
  -d '{"project": "envs/prod", "action": "plan", "wait": true, "by": "github-actions"}'
```

- `project`: a project id (`terraform:envs/prod`) or its name / folder.
- `action`: defaults to the safe one. `"action": "apply", "plan": "latest"`
  applies the project's newest plan (or `"plan": "<run id>"`).
- `options`: the same as [run options](#run-options) (`{"workspace": "staging"}`).
- Without `wait`: `202` and the run id. With `"wait": true`: the finished run,
  **200** if it succeeded, **409** if it failed, was cancelled, or waits for
  approval. `curl --fail` then fails the CI step.

## Pull and push webhooks

- **Pull** fetches the branch and finds the projects again (new folders and
  playbooks appear, removed ones disappear).
- **Webhook** (*Repository → Setup*): Payload URL
  `https://<timika>/v1/automation/hooks/<repo>`, content type
  `application/json`, the secret shown there, *just the push event*. GitHub and
  Gitea send `X-Hub-Signature-256` (HMAC-SHA256), GitLab sends `X-Gitlab-Token`.
  Both are checked in constant time. Unsigned or wrongly signed deliveries get
  `401`.
- A push to the tracked branch **pulls**. With *plan on push* it also starts
  **Plan / Check / Preview** for each project the push touched, by changed
  file:
  - A Terraform / Pulumi project when a file in its folder changed.
  - A playbook when a file under its folder changed that isn't inside a
    Terraform / Pulumi folder.
  - Everything when the push doesn't say (force-push).
- Pushes never apply. Applying stays a person's decision: open the plan and
  press **Apply**.
- The *Setup* tab shows the last delivery: when, which commit, who pushed, and
  what started.

GitHub / GitLab must be able to reach timika. If it isn't public, skip the
webhook and press **Pull**.

## Who can do what

| | Admin | Vault reader (`read-only`) | `ssh` role |
|---|---|---|---|
| See repositories, runs, output | ✓ | ✓ | — |
| See the webhook secret | ✓ | — | — |
| Connect / edit / remove, Pull, Plan, Apply, Cancel, schedules, notifications, browse code | ✓ | — | — |
| Approve someone else's Apply / Run / Up | ✓ (not your own) | — | — |

Everything is in the **audit trail** (category *Automation*): connected or
changed repositories, pulls, deploy keys, every run started (project, action
and run id), cancels and webhook deliveries (accepted or refused). Secret
values never appear in it.

## Settings

| Variable | Default | |
|---|---|---|
| `AUTOMATION_DIR` | `./data/automation` (image: `/data/automation`) | Bare clones (needs `git` and `ssh` — in the image) |
| `AUTOMATION_RUN_TIMEOUT_MIN` | 60 | A run is stopped after this |
| `AUTOMATION_LOG_MAX_MB` | 20 | Output kept per run (the rest is cut, the run continues) |
| `AUTOMATION_RETENTION_DAYS` | `SESSION_RETENTION_DAYS` (90) | Finished runs, output and plans are deleted after this |
| `PUBLIC_URL` | — | timika's address, for links in notifications |
| `AUTOMATION_SCHEDULE_TICK_SECS` | 20 | How often schedules are checked |
| `AUTOMATION_ALLOW_FILE_URLS` | off | Allow `file://` repositories (tests only) |

## Security notes

- Git runs with no system or user config, no hooks, only `https` / `http` /
  `ssh` remotes (no `ext::`, no `file://`), no prompts, and a time limit. SSH
  host keys of Git servers are trusted on first use, per repository.
- Runs go to the runner over the bastion's SSH (pinned host key). The runner
  sees the code and the variables; choose one you'd trust with your cloud
  credentials (a dedicated CI box).
- Plan files and state can contain secrets: both are barrier-encrypted, like
  everything else in the vault.
- One run per project is enforced by timika. Terraform's own state locking (with
  a backend) still applies across tools.

## Limits (today)

- Runner servers only (no Kubernetes / container runners yet).
- Not yet: Terragrunt, shell / Python script projects, build → deploy
  versioning, workflows chaining several projects, IANA time zones in
  schedules.
- Crossplane / Kubernetes manifests aren't detected as projects yet.
