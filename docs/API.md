# API — gRPC

The API is gRPC, defined in [`proto/timika/v1/*.proto`](../proto/timika/v1) (package
`timika.v1`). One port (8200) serves:

| Protocol | Who uses it | Path |
|----------|-------------|------|
| **gRPC** (HTTP/2) | services, the `timika operator` CLI, `grpcurl`, `buf curl` | `/timika.v1.<Service>/<Method>` |
| **gRPC-Web** (HTTP/1.1) | the browser UI (Connect `createGrpcWebTransport`) | same paths |
| `grpc.health.v1.Health` | gRPC-aware probes / LBs (`SERVING` only while unsealed) | `/grpc.health.v1.Health/Check` |
| plain HTTP | load balancers and k8s probes that can't speak gRPC | `GET /v1/sys/health` |
| WebSocket | the web terminal (browsers can't do bidirectional gRPC streams); `container=<name>` opens a shell inside a container (admins) | `GET /v1/bastion/connect` |
| HTTP webhook | push webhooks from GitHub / GitLab / Gitea (repository webhook secret) | `POST /v1/automation/hooks/<repo>` |
| HTTP streams | files in a container | `PUT /v1/containers/upload?asset&container&path` (token header) · `GET /v1/containers/download?t=<link>` |
| HTTP trigger | CI starts a run (`Authorization: Bearer tmkci.…`; `wait` → 200 / 409) | `POST /v1/automation/trigger/<repo>` |
| HTTP streams | file transfer (gRPC-Web can't stream request bodies) | `PUT /v1/bastion/files/upload?asset&account&path` (token header) · `GET /v1/bastion/files/download?t=<link from DownloadLink>` |

No proxy (Envoy etc.) is needed for gRPC-Web — the server speaks it natively.

## Authentication

Send the token as call metadata: `x-timika-token: tmk.…` (or `authorization: Bearer tmk.…`).

| Role | Can call |
|------|----------|
| none | `SysService` Health · GetSealStatus · Init · Unseal · Instances, `AuthService` GetLoginConfig · GetCaptchaChallenge · Login · VerifyMfa (with the `mfa_challenge` from Login), `ClusterService` JoinChallenge · JoinAnswer |
| any session (incl. the restricted *change-password* and *mfa-setup* sign-ins) | `AuthService` LookupSelf · RenewSelf · RevokeSelf · ChangePassword · GetMfaStatus · BeginMfaSetup · ConfirmMfaSetup (an *mfa-setup* session comes back as a full one) · DisableMfa · NewRecoveryCodes |
| `ssh` | only the bastion: ListAssets (servers granted to them), the terminal, files, ListSessions/GetRecording/ListCommands (their own) — no vault data |
| `read-only` | the `ssh` rights plus reads: KV List/Read/GetMetadata, GetKeyStatus, Storage, Audit, Configuration, AutomationService ListRepos/GetRepo/ListRuns/GetRun/WatchRun |
| `admin` / root | everything |

## Errors → status codes

| Code | When |
|------|------|
| `UNAUTHENTICATED` (16) | missing, unknown or expired token; disabled user |
| `PERMISSION_DENIED` (7) | role too low, password change or 2FA setup required (restricted session), captcha failed, wrong 2FA code, account locked |
| `FAILED_PRECONDITION` (9) | `vault is sealed` · `vault is not initialized` |
| `INVALID_ARGUMENT` (3) | bad path, weak password, missing field |
| `NOT_FOUND` (5) | secret / version / user / server not found |
| `ABORTED` (10) | check-and-set mismatch, concurrent update — retry; a project already running; a plan already applied |
| `ALREADY_EXISTS` (6) | a server with that name exists |
| `RESOURCE_EXHAUSTED` (8) | sign-in rate limit |
| `UNAVAILABLE` (14) | storage unreachable, no Raft leader, audit log down (fail-closed) |

## Services

| Service | RPCs |
|---------|------|
| `SysService` | Health · GetSealStatus · Init · Unseal (`all`: every instance; `reset`) · Instances · Seal (`all`) · Rotate · GetKeyStatus · Storage · Audit · AuditHash |
| `KvService` | List · Read (`version`) · Write (`cas`) · Delete (soft, `versions`) · GetMetadata · Destroy |
| `AuthService` | GetLoginConfig · GetCaptchaChallenge · Login (→ a session, or `mfa_required` + `mfa_challenge`, or a restricted `mfa-setup` session when 2FA is required but not set up) · VerifyMfa (TOTP or recovery code; challenge lasts 5 min / 5 tries) · LookupSelf · RenewSelf · RevokeSelf · ChangePassword · GetMfaStatus · BeginMfaSetup (secret + QR SVG) · ConfirmMfaSetup (→ 10 recovery codes) · DisableMfa · NewRecoveryCodes · ListUsers · GetUser · UpsertUser · DeleteUser · UnlockUser · ResetUserMfa (admin) |
| `BastionService` (files) | ListFiles · MakeDir · RenameFile · DeleteFiles · DownloadLink (one-minute signed link) · Compress (zip / tar.gz / tar.xz / tar, made by the server's own tar / zip) · Extract (into a new folder) · ArchiveLink (stream a folder / selection as one archive) — SFTP as an account you may use; uploads capped by `FILES_MAX_UPLOAD_MB` (4096) |
| `BastionService` | ListAssets · CreateAsset (`test`) · UpdateAsset · DeleteAsset · TestAsset · ResetHostKey · ListGrants · ListAllGrants · CreateGrant · DeleteGrant · ListSessions · KillSession · GetRecording (server stream, asciicast v2) · ListCommands (per server or session; `query`, `risky`) |
| `BastionService` (accounts on the VM) | `AccountInput.provision` creates/updates the Linux user over SSH before saving: `provision_via` (an account on the same server, e.g. `root`, or the account itself), `password` (set with `chpasswd`), `sudo` (`keep` · `none` · `password` · `nopassword`, a `visudo`-checked file in `/etc/sudoers.d`), `groups` (`usermod -aG`; missing groups are reported, not created) |
| `ClusterService` | Configuration · RemovePeer · Snapshot (server stream) · JoinChallenge · JoinAnswer |
| `AutomationService` | ListRepos · GetRepo (+ recent commits) · NewDeployKey · CreateRepo (clones now; `branch` empty = default branch) · UpdateRepo (empty token / key / secret value = keep) · DeleteRepo · SyncRepo (git pull) · CheckRunner (tools on the runner) · ListRuns · GetRun · StartRun (`plan` · `apply` with `plan_run` · `check` · `run` · `preview` · `up`) · CancelRun · WatchRun (server stream: output from the start, then live) · ApproveRun / RejectRun (another admin) · ListRepoFiles / ReadRepoFile (browse the clone at a commit; admin) · ListSchedules · SaveSchedule · DeleteSchedule · RunScheduleNow · SetRepoPolicy (approvals) · SaveNotifiers · TestNotifier · RotateTriggerToken. StartRun takes `options` (limit, tags, skip_tags, extra_vars, verbose, servers, destroy, targets, replace, workspace, refresh) — [AUTOMATION.md](AUTOMATION.md) |
| `MonitorService` | ListSystems (your servers' latest numbers; admins also get the unmonitored ones) · GetSystem (`range`: 1h · 6h · 24h · 7d · 30d · 90d → times + series) · SetMonitoring · SetSystemAlerts · ListContainers · GetContainerUsage (one container: latest numbers + history for `range`) · ContainerLogs / ContainerAction (start · stop · restart; admin) · GetSettings / SaveSettings (default rules, notifiers) · TestNotifier — [MONITORING.md](MONITORING.md) |
| `ContainerService` (admin; Docker or Podman) | InspectContainer · ListFiles · MakeDir · DeleteFiles · DownloadLink (a file, or a folder as .tar) · ListImages · RemoveImage · PruneImages · ListVolumes · RemoveVolume · PruneVolumes — [CONTAINERS.md](CONTAINERS.md) |
| `AuditService` | ListEvents (newest first; filter by user, text, category incl. `files`, outcome, `server`; routine reads hidden unless `include_routine`) · Verify (hash chain) — admin |

Times are RFC 3339 strings. Free-form reports (`Storage`, `Audit`, `SealResponse.report`)
are `google.protobuf.Struct`.

## Examples

```bash
# The CLI built into the image (gRPC under the hood)
timika operator init --shares 5 --threshold 3
timika operator unseal --all
TIMIKA_TOKEN=tmk.… timika operator seal --all

# grpcurl / buf curl, using the .proto files (no server reflection)
grpcurl -plaintext -import-path proto -proto timika/v1/kv.proto \
  -H "x-timika-token: $TOKEN" -d '{"path":"app/db","data":{"password":"s3cret"}}' \
  localhost:8200 timika.v1.KvService/Write
buf curl --schema proto --protocol grpc --http2-prior-knowledge \
  -H "x-timika-token: $TOKEN" -d '{"path":"app/db"}' \
  http://localhost:8200/timika.v1.KvService/Read
```

## Generated clients

- **Rust:** `backend/build.rs` (tonic-build) → `crate::grpc::pb`.
- **TypeScript:** `cd frontend && bun run gen` (buf + protoc-gen-es) → `frontend/src/gen`,
  used through `frontend/src/lib/api.ts`.

Change a `.proto` → rebuild the backend and rerun `bun run gen`.

## Clustering

Every node serves every RPC. With Raft, a follower makes its reads linearizable
(read-index from the leader) and forwards writes and membership changes to the
leader over the signed cluster port — clients never need to find the leader.

## Through a load balancer

gRPC-Web and the terminal WebSocket are HTTP/1.1; native gRPC needs HTTP/2 to the
server. `deploy/scale/haproxy.cfg` routes on `content-type: application/grpc` to
`proto h2` backends and keeps the rest on HTTP/1.1 (`timeout tunnel` for long-lived
WebSockets/streams). Behind TLS, advertise `alpn h2,http/1.1`. Ingress controllers:
use a gRPC-capable backend protocol (e.g. nginx `backend-protocol: GRPC`) for native
clients, or keep HTTP/1.1 if only the UI (gRPC-Web) goes through it.
