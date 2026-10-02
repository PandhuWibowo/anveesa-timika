# CLAUDE.md

Guidance for Claude Code (and other AI agents) working in this repository.

## What this is

**anveesa-timika** is a Vault-style secrets manager. A Rust backend (axum) holds the
encryption barrier and seal logic over **pluggable storage**:

- `STORAGE=redis`: Redis is the database (fast path), in `REDIS_MODE=standalone|sentinel|cluster`.
- `STORAGE=raft`: integrated Raft (openraft + redb). Every node has a full copy.
  HA on Kubernetes or VMs, with no external database.

The Svelte 5 frontend uses the anveesa design system ported from sibling
`anveesa-mikko` (itself from `anveesa-nias`).

The API is **gRPC** (`proto/timika/v1/*.proto`, tonic + tonic-web): native gRPC
and gRPC-Web on one port. The only plain-HTTP endpoints are `GET /v1/sys/health`
and the web-terminal WebSocket `GET /v1/bastion/connect` (docs/API.md).

Read [`docs/API.md`](docs/API.md), [`docs/PERSISTENCE.md`](docs/PERSISTENCE.md), [`docs/REDIS.md`](docs/REDIS.md), [`docs/RAFT.md`](docs/RAFT.md),
[`docs/SCALING.md`](docs/SCALING.md), [`docs/AUTO-UNSEAL.md`](docs/AUTO-UNSEAL.md), [`docs/LOGGING.md`](docs/LOGGING.md) and [`docs/LOGIN.md`](docs/LOGIN.md)
before touching storage, crypto or clustering.

## 🛑 Security invariants — do not break

1. **Nothing but `core/seal-config` (and, with auto-unseal, the KMS-wrapped
   `core/root-key-wrapped`) may be stored in plaintext.** All writes go
   through `Core::commit` (barrier), never `Storage` directly. The two
   exceptions are the keyring (root-key encrypted) and seal-config. Raft log
   entries carry the same ciphertext.
2. **The root key is never persisted or logged.** It exists only in `Core.unsealed`
   and in the shares returned once by `sys/init`.
3. **Keep the storage path as AEAD associated data.** It stops ciphertext from being
   moved between keys.
4. **Shamir: a restart comes back sealed.** Auto-unseal (`seal/`, docs/AUTO-UNSEAL.md)
   only ever unwraps through an external key service — never persist the root
   key or the key-service credentials. An explicit seal/seal-all must keep auto-unseal
   paused until restart (`sealed_on_purpose`).
5. Token lookups use `sha256(token)` as the key. Never store raw tokens.
6. **Raft only runs while unsealed**, and every cluster RPC must pass
   `cluster_auth::verify` (HMAC keyed from the root key). Don't add unauthenticated
   routes to the cluster listener.
7. **Audit log: never log request/response bodies or raw tokens** (tokens only
   via `Core::audit_hmac`). Name what a call acts on with `audit::target(...)` —
   identifiers only (paths, user/server names), never values or passwords. Keep it fail-closed by default, the `request` entry
   written *before* the handler runs, and the hash chain intact (`audit.rs`).
8. **Sign-in:** passwords only as Argon2id hashes; keep the dummy-hash timing
   equalizer, the captcha-before-password order, guarded lockout counters, no
   session before the second factor (auth/mfa.rs: TOTP replay protection, wrong
   codes count toward lockout), and
   never return captcha/KMS secrets from `login-config` (docs/LOGIN.md).
9. **Infrastructure code never runs on the timika host** — only on a runner
   server over SSH. Git gets tokens via env headers and keys via 0600 temp files,
   never on a command line; secret variables are masked in output and never
   returned by the API (automation/, docs/AUTOMATION.md).
10. **Joining requires the challenge.** A node becomes a voter only via
   `join_answer` after decrypting a barrier-encrypted nonce. Keep the single-use and
   TTL checks.

## Commands

```bash
make redis        # local Redis :6380 (AOF everysec, noeviction) — idempotent
make dev          # backend :8200 (STORAGE from backend/.env) + frontend :5174
make raft-dev     # 3-node Raft cluster on :8200/:8210/:8220 (make raft-reset wipes)
make raft-up      # same cluster in docker compose
make redis-sentinel-up / redis-cluster-up   # Redis HA rigs with timika inside
make stop         # free the dev ports
make gen          # regenerate frontend/src/gen from proto/ (after editing a .proto)
make test         # gen + cargo test + svelte-check
make e2e          # 306 end-to-end scenarios on real processes (frontend/e2e, ~1 min)
make helm-lint    # lint deploy/helm/timika
```

After a backend change, restart it. Rust doesn't hot-reload.

## Layout

```
backend/src/
  main.rs         wiring; picks the storage backend; starts API + (raft) cluster listener
  config.rs       env Config (STORAGE, REDIS_*, RAFT_*, API/CLUSTER addrs, TLS_*)
  tls.rs          outbound client (CA trust) + optional TLS serving for both listeners
  storage/        mod.rs: Storage enum {Redis, Raft} — get/get_local/commit/list/describe
                  redis.rs: Redis backend — one Conn enum for standalone/Sentinel/Cluster,
                  `{prefix}` hash-tag keys, Lua commit + sorted-set index, WAIT,
                  Sentinel failover watcher, durability/topology report
  raft/           mod.rs: RaftBackend (lifecycle, linearizable reads, membership, cluster_auth)
                  store.rs: redb log store + state machine + snapshots
                  network.rs: raft RPC client · server.rs: cluster listener (/raft/*)
                  types.rs: TypeConfig, Request, ClusterNode
  barrier.rs      AES-256-GCM blobs [ver|term|nonce|ct], keyring with terms (+ unit tests)
  core.rs         init / unseal (Shamir) / seal / rotate / keyring refresh / raft join
  token.rs        token entries, resolve() (expiry, disabled users)
  grpc/           the API: mod.rs (Ctx, who()/Need roles, AppError→Status, router with
                  tonic-web + grpc.health), sys/kv/auth/bastion/cluster/audit.rs services,
                  convert.rs (JSON↔Struct, times), client.rs (node→node + CLI calls)
  seal/           auto-unseal key services: transit (Vault/OpenBao), awskms (SigV4 +
                  credential chain), static; AutoSeal::wrap/unwrap
  logging.rs      operational logs: stdout + rotating JSON files (tracing-appender)
  audit.rs        audit log: middleware, hash chain, file (rotation) + syslog sinks, verify
  auth/           sign-in: policy.rs (us-nist / cn-mlps profiles), userpass.rs (accounts,
                  lockout, sessions, MFA challenge), mfa.rs (TOTP RFC 6238 + recovery codes),
                  captcha.rs (pow + turnstile/recaptcha/hcaptcha/geetest/tencent)
  cli.rs          `timika operator …` built-in CLI over gRPC (status/instances/init/unseal --all/seal --all/health)
  bastion/        servers, credentials, grants, SSH (russh), recorded sessions (asciicast),
                  commands.rs: per-session command log (only what the screen echoed — never hidden input)
                  files.rs: SFTP pool + signed download links · archive.rs: tar/zip commands (every name
                  quoted + ./-prefixed) · provision.rs: create accounts, sudo, groups
  automation/     infrastructure from Git (docs/AUTOMATION.md): mod.rs (repos, runs, project
                  detection, summaries, secret Masker, start_run/sync_repo), git.rs (hardened
                  `git` on bare clones, deploy keys), runner.rs (snapshot → runner over SFTP,
                  `sh run.sh` over SSH, output/plan/state back into the vault, server inventory),
                  schedule.rs + cron.rs (cron schedules, claimed once cluster-wide), notify.rs
                  (Slack/Teams/Discord/Telegram/webhook)
  monitor/        agentless monitoring (docs/MONITORING.md): collect.rs (the sh script run over
                  SSH + its parser, rates), engine.rs (one collecting instance, history
                  roll-ups, alert rules, notifications), mod.rs (storage, ranges)
  kv.rs           KV v2 engine: versions, CAS, soft delete, destroy, folder listing
  routes/         plain HTTP only: /v1/sys/health, the /v1/bastion/connect WebSocket,
                  /v1/bastion/files/upload (PUT) and /download (GET, signed link),
                  /v1/automation/hooks/<repo> (push webhooks, HMAC / token), /v1/automation/trigger/<repo> (CI)
proto/timika/v1/  the API contract (sys, kv, auth, bastion, cluster, audit, automation, monitor)
frontend/src/
  App.svelte      shell (sidebar/topbar/statusbar) or Gate when sealed/uninit/no token
  gen/            generated from proto (`bun run gen`: buf + protoc-gen-es) — don't edit
  lib/            api.ts (Connect gRPC-Web clients, errMsg/isNetworkError), query.ts (TanStack
                  Query client + keys; memory-only cache), session/router/ui
                  (.svelte.ts rune stores), nav.ts, roles.ts, password.ts, auditText.ts, types.ts
  components/     CommandPalette (⌘K, incl. "connect to …"), ConfirmModal, ServerDrawer +
                  AccountCard (accounts + provisioning), UserDrawer, SessionsTable, Replay,
                  FilesBrowser (SFTP + archives), MfaSetup, Captcha
  views/          Gate, Login (password → 2FA), Overview, Servers, ServerDetail (Commands ·
                  Files · Sessions · Activity · Access), Terminals (always mounted — sessions
                  survive navigation; lib/terminals.svelte.ts), Sessions, People, Audit, Account,
                  Automation / AutomationRepo / RunView (+ components RepoDrawer, RunsTable, RunDialog,
                  RunOptionsForm, ScheduleDialog, NotifySettings), Monitoring / MonitorSystem
                  (+ Chart, RulesEditor, MonitorSettings, ContainerPanel, ContainerLogs), Containers
deploy/helm/timika/   backend=raft → StatefulSet + headless svc; backend=redis → Deployment
deploy/scale/         compose scale-out + seal-aware haproxy.cfg
deploy/swarm/         Swarm stacks (redis / raft)
deploy/redis/         Redis Sentinel + Cluster rigs
scripts/raft-dev.sh   local 3-node cluster
```

## Scale-out rules (docs/SCALING.md)

- Assume **N timika instances** share storage. Any read-modify-write must use
  `Core::get_json_guarded` + a guarded `Core::commit` inside a retry loop
  (`kv::retrying`). `write_lock` is only a local optimization, not correctness.
- Seal state is per instance. Anything that must reach every replica goes
  through the registry/marker (Redis) or a signed cluster RPC (Raft), like seal-all.
- The container `HEALTHCHECK` / liveness must never depend on seal state; only
  readiness and the LB's `ready` pool do.

## Conventions

- **Match the surrounding code.** Handlers are thin; logic lives in `core.rs` / `kv.rs`.
- Every Redis key must keep the `{prefix}` hash tag (Cluster atomicity) and go through the
  Lua commit so the index stays in sync. Key-less commands use `on_primary` (slot routing).
- Multi-key writes go in **one** `Core::commit` (one Redis Lua script or one Raft entry),
  so they're atomic.
- Read-modify-write uses guarded commits (CAS) in a retry loop; `write_lock` is only
  a local optimization. With Raft, followers forward writes to the leader, and
  guards are checked when the entry is applied, so CAS holds cluster-wide.
- New API: add the RPC to the `.proto`, implement it in `grpc/<service>.rs` starting
  with `self.who(&req, Need::…)` (or `self.public()`), return `reply(msg, &me)` so the
  audit log knows the caller, then `bun run gen` for the UI. Don't add REST routes.
- New backend features must work on **both** storage backends (`storage/mod.rs`).
- Frontend: reuse the `.page-*` / `.base-*` / `.badge--*` / `.notice--*` classes in
  `style.css`; don't invent new component CSS. Svelte 5 runes only.
- New behaviour gets an end-to-end scenario in `frontend/e2e/scenarios/` (one file
  per area; `scenario(cat, title, fn)`), and `make e2e` must stay green.
- Frontend data: lists and detail reads go through **TanStack Query** (`lib/query.ts`:
  `createQuery(() => ({ queryKey: keys.…, queryFn, refetchInterval }))`), not hand-rolled
  `setInterval` + `$state`. After a change, `refetch()` / `queryClient.invalidateQueries`.
  The cache is memory-only and cleared on sign-in, sign-out and seal — never persist it.
  Streams (terminal, run output) stay as they are.
- New page: add `views/X.svelte`, register it in `App.svelte`'s `views` map, and add
  a nav entry in `lib/nav.ts` (the command palette picks it up automatically).

## Roadmap (not built yet)

Policies (path ACLs) + non-root tokens with TTL · rekey (new shares) ·
snapshot **restore** endpoint · autopilot (dead-server cleanup) · GCP/Azure KMS seals ·
seal migration back to Shamir / between KMSs · more secret engines (transit, dynamic DB creds) ·
bastion: command capture for multi-line pastes, in-browser file editor, folder upload ·
infrastructure: container / Kubernetes runners, Terragrunt + script projects, workflows (chained
projects), IANA time zones, Crossplane / Kubernetes manifests, GitHub App instead of tokens / deploy keys.
