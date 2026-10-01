# Logging

timika writes two different logs:

| | Operational log | Audit log |
|---|---|---|
| Answers | What is the service doing? (startup, storage, Raft, seal events, failures) | Who did what, to which path, from where, with what result? |
| Format | JSON lines (files); text or JSON on stdout | JSON lines, one `request` + one `response` entry per API call, hash-chained |
| Written to | stdout + rotating files | rotating files, syslog, and optionally stdout |
| Retention | time-based rotation (`LOG_ROTATION`, `LOG_MAX_FILES`) | size-based rotation (`AUDIT_FILE_MAX_MB`, `AUDIT_FILE_MAX_FILES`) |
| If it can't be written | the service keeps running | the request is **refused** (fail-closed) |

The image turns both on by default and writes to `/var/log/timika`. Mount a
volume there; every compose file, Swarm stack and the Helm chart already do.
Clear `LOG_DIR` or `AUDIT_FILE` to disable either one.

## Audit trail in the UI

**Access → Audit trail** (admins) reads this instance's audit file back through
`AuditService.ListEvents`: one row per call (request + response joined) or terminal
session, with who, what, the **target** (a secret path, user or server name — never
a value), the source address and the outcome. Successful routine reads (session
keep-alives, list views, polling) are hidden by default; failures always show.
**Verify integrity** runs the same hash-chain check as `operator audit-verify`.
In a cluster each instance keeps its own file: the page shows the instance you
reached — ship files to your SIEM for one merged view.

## Operational log

| Env | Default | |
|---|---|---|
| `LOG_LEVEL` | `info,anveesa_timika_backend=debug,openraft=warn` | filter (falls back to `RUST_LOG`) |
| `LOG_FORMAT` | `text` | stdout format: `text` or `json` |
| `LOG_DIR` | `/var/log/timika` (image) | enable files here, always JSON |
| `LOG_ROTATION` | `daily` | `daily` · `hourly` · `never` |
| `LOG_MAX_FILES` | `14` | older rotated files are deleted |
| `LOG_FILE_PREFIX` | `timika-{instance}` | file name: `timika-<instance>.2026-10-01.log` |

Writes go through a background, non-blocking writer, so slow disks don't slow
requests down.

## Audit trail in the UI

**Access → Audit trail** (admins) reads this instance's audit file back through
`AuditService.ListEvents`: one row per call (request + response joined) or terminal
session, with who, what, the **target** (a secret path, user or server name — never
a value), the source address and the outcome. Successful routine reads (session
keep-alives, list views, polling) are hidden by default; failures always show.
**Verify integrity** runs the same hash-chain check as `operator audit-verify`.
In a cluster each instance keeps its own file: the page shows the instance you
reached — ship files to your SIEM for one merged view.

## Audit log

Each API call produces two entries that share an `id`. That id is also
returned to the client as the `x-timika-request-id` header.

```json
{"type":"request","id":"7bf7…","time":"…","instance":"app-1","remote_addr":"10.0.3.7",
 "auth":{"token_hmac":"hmac-sha256:73b5…"},
 "request":{"protocol":"grpc-web","rpc":"timika.v1.KvService/Write"},
 "seq":5,"prev_hash":"…","hash":"…"}
{"type":"response","id":"7bf7…", …,
 "auth":{"token_hmac":"hmac-sha256:73b5…","display_name":"root","policies":["root"]},
 "response":{"status":200,"error":null,"duration_ms":3,"served_by":null}, …}
```

**What is never logged:**
- **Request and response bodies.** They carry secret values and unseal keys.
  Verified: a secret value, the root token and an unseal key appear 0 times
  across all log files.
- **Tokens in clear text.** Only `hmac-sha256:…` appears. The HMAC key is derived
  from the root key, so it's the same on every instance. To find a token's entries:
  ```bash
  timika operator audit-hash tmk.XXXX        # → hmac-sha256:…
  grep 'hmac-sha256:…' /var/log/timika/audit-*.log
  ```

**What is logged:**
- **Paths are plaintext** (same as Vault). They are secret *names*, not values;
  don't put sensitive data in names.
- **Load-balancer and probe polling is skipped:** `GET /v1/sys/health`, `SysService` Health,
  `/sys/seal-status` and `/sys/instances`.
- **Raft forwarding:** a request that a follower forwards to the leader is
  logged by both nodes under the **same id**. The leader's entry carries
  `forwarded_by`.

### Audit trail in the UI

**Access → Audit trail** (admins) reads this instance's audit file back through
`AuditService.ListEvents`: one row per call (request + response joined) or terminal
session, with who, what, the **target** (a secret path, user or server name — never
a value), the source address and the outcome. Successful routine reads (session
keep-alives, list views, polling) are hidden by default; failures always show.
**Verify integrity** runs the same hash-chain check as `operator audit-verify`.
In a cluster each instance keeps its own file: the page shows the instance you
reached — ship files to your SIEM for one merged view.

## Fail-closed

The `request` entry is written *before* the handler runs. If no sink can record
it, the request is refused with `503 audit log unavailable` and nothing
happens. Verified: an unseal attempt was refused and the vault stayed sealed.
A request succeeds if **at least one** sink records it, and failing sinks are
reported in the operational log. `AUDIT_FAIL_CLOSED=false` switches to
fail-open, where requests are served and the failure is only logged.

### Audit trail in the UI

**Access → Audit trail** (admins) reads this instance's audit file back through
`AuditService.ListEvents`: one row per call (request + response joined) or terminal
session, with who, what, the **target** (a secret path, user or server name — never
a value), the source address and the outcome. Successful routine reads (session
keep-alives, list views, polling) are hidden by default; failures always show.
**Verify integrity** runs the same hash-chain check as `operator audit-verify`.
In a cluster each instance keeps its own file: the page shows the instance you
reached — ship files to your SIEM for one merged view.

## Tamper evidence

Every entry contains `seq`, `prev_hash` and `hash` (sha256 of the entry). This
chains the whole log, across rotated files and restarts; the chain resumes from
the last line on startup.

```bash
timika operator audit-verify /var/log/timika/audit-app-1.log   # includes rotated .1 .2 … siblings
```

| Case | Result |
|---|---|
| edited entry | `content does not match its hash (entry was modified)` |
| deleted line, or a missing rotated file in the middle | `prev_hash does not match the previous entry` |
| intact | `OK`; exit code 0, otherwise 1 |

A hash chain proves consistency, not authenticity. Someone who can rewrite the
*whole* file could rebuild a valid chain. Defend against that by keeping a
second copy somewhere they can't write: `AUDIT_SYSLOG` to a central collector.
Each entry travels with its hash, so the two copies can be compared.

### Audit trail in the UI

**Access → Audit trail** (admins) reads this instance's audit file back through
`AuditService.ListEvents`: one row per call (request + response joined) or terminal
session, with who, what, the **target** (a secret path, user or server name — never
a value), the source address and the outcome. Successful routine reads (session
keep-alives, list views, polling) are hidden by default; failures always show.
**Verify integrity** runs the same hash-chain check as `operator audit-verify`.
In a cluster each instance keeps its own file: the page shows the instance you
reached — ship files to your SIEM for one merged view.

## Sinks

| Env | |
|---|---|
| `AUDIT_FILE` | path; `{instance}` → this instance's id. Default `/var/log/timika/audit-{instance}.log` |
| `AUDIT_FILE_MAX_MB` / `AUDIT_FILE_MAX_FILES` | rotate at 100 MB, keep 10 files (`audit.log.1` … `.10`) |
| `AUDIT_FILE_FSYNC` | `true` = fsync each entry (survives power loss; slower) |
| `AUDIT_SYSLOG` | `udp://host:514` or `tcp://host:601`. RFC 5424, facility authpriv; TCP uses octet-counting framing and reconnects |
| `AUDIT_STDOUT` | `true` = also print entries to stdout |
| `AUDIT_FAIL_CLOSED` | default `true` |

`SysService/Audit` (token) shows this instance's sinks, mode, sequence and
chain head.

## Audit trail in the UI

**Access → Audit trail** (admins) reads this instance's audit file back through
`AuditService.ListEvents`: one row per call (request + response joined) or terminal
session, with who, what, the **target** (a secret path, user or server name — never
a value), the source address and the outcome. Successful routine reads (session
keep-alives, list views, polling) are hidden by default; failures always show.
**Verify integrity** runs the same hash-chain check as `operator audit-verify`.
In a cluster each instance keeps its own file: the page shows the instance you
reached — ship files to your SIEM for one merged view.

## Persistence by deployment

| Deployment | Where logs survive |
|---|---|
| `docker compose` (single) | the `timika-logs` volume. `INSTANCE_ID` is pinned, so file names, and the audit chain, survive container re-creation. Verified: the chain resumed across `down`/`up`. |
| scaled compose (`deploy/scale`) | one shared volume; each replica writes its own files (`audit-<instance>.log`) |
| Raft (compose / Swarm / Helm) | a log volume per member; Helm uses a PVC per pod |
| Swarm, Redis stack | a per-node volume. Add `AUDIT_SYSLOG` for a trail that doesn't depend on one node |
| Helm, `backend=redis` | `logging.persistence.existingClaim` (ReadWriteMany), otherwise an `emptyDir` that dies with the pod. **Set `logging.audit.syslog` there.** |

Files are per instance so replicas never write to the same file. With
container-generated ids (scaled replicas), each new container starts its own
file and chain, which `audit-verify` reports as a new chain start.
