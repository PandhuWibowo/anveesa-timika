# anveesa-timika

A Vault-style **secrets manager**: a Rust encryption barrier in front of
pluggable storage. Use **Redis** (standalone, Sentinel or Cluster) for speed, or
**integrated Raft** for HA on Kubernetes or VMs with no external database. The Svelte UI uses
the anveesa design system (ported from anveesa-mikko).

*Secrets at rest, keys in memory.*

```
 Svelte 5 SPA ──/v1──▶ timika (Rust / axum)
                        ├─ token auth
                        ├─ KV v2 engine (versions, CAS, soft delete)
                        ├─ barrier: AES-256-GCM, keyring with terms
                        └─ seal: Shamir-split root key, memory only
                              │ ciphertext only
                              ▼
          STORAGE=redis: Redis standalone · Sentinel · Cluster (AOF, noeviction, optional WAIT)
          STORAGE=raft:  3/5 timika nodes, each with a redb copy, replicated by Raft
```

## Quick start

```bash
make redis     # Redis on :6380 with AOF + noeviction (docker)
make install   # frontend deps (bun)
make dev       # backend :8200 + frontend :5174
```

Open http://localhost:5174. Initialize (5 shares / threshold 3), save the keys,
submit 3 of them to unseal, and you're in. Restart the backend: the data is still
in Redis, but the vault is **sealed** again until 3 keys are entered.

### Redis Sentinel / Cluster

```bash
make redis-sentinel-up   # primary + 2 replicas + 3 sentinels + timika on :8200
make redis-cluster-up    # 6-node Redis Cluster + timika on :8200
```

Set `REDIS_MODE=sentinel|cluster`; see [`docs/REDIS.md`](docs/REDIS.md) for
configuration, `REDIS_WAIT_REPLICAS`, and measured failover times.

### Raft cluster (HA)

```bash
make raft-dev    # 3 nodes: :8200, :8210, :8220 (no Docker)
make frontend    # UI → timika-0
```

Initialize timika-0, unseal it, then unseal timika-1 and timika-2 with the same
keys. They join automatically. Kill the leader and a new one takes over within
about 2s. Containers: `make raft-up`. Kubernetes: `deploy/helm/timika`. VMs and
systemd: [`docs/RAFT.md`](docs/RAFT.md).

### Scale out (N replicas: plain Docker, Swarm, Kubernetes)

```bash
make scale-up REPLICAS=3         # Redis + 3 timika replicas + seal-aware HAProxy on :8200
docker compose -f deploy/scale/docker-compose.yml exec timika /app/timika operator init
docker compose -f deploy/scale/docker-compose.yml exec timika /app/timika operator unseal --all
```

Replicas coordinate through guarded (compare-and-swap) commits, a live instance
registry, and seal-all. Add **auto-unseal** (`SEAL_TYPE=transit|awskms|static`, see
[`docs/AUTO-UNSEAL.md`](docs/AUTO-UNSEAL.md)) and new replicas unseal themselves. See [`docs/SCALING.md`](docs/SCALING.md) for Swarm
stacks and the Helm chart (`backend=redis` Deployment or `backend=raft` StatefulSet).

### CLI (built into the image)

```bash
timika operator status | instances | init | unseal [--all] | seal [--all] | health
timika operator audit-verify /var/log/timika/audit-<instance>.log | audit-hash <token>
```

## API

gRPC (`proto/timika/v1`): native gRPC for services and the CLI, gRPC-Web for the
browser UI, on the same port. Plain `GET /v1/sys/health` stays for load balancers,
and the web terminal is a WebSocket. Full reference: [`docs/API.md`](docs/API.md).

```bash
grpcurl -plaintext -import-path proto -proto timika/v1/kv.proto \
  -H "x-timika-token: $TOKEN" -d '{"path":"app/db","data":{"password":"s3cret"}}' \
  localhost:8200 timika.v1.KvService/Write
```

With Raft, any node accepts any call; followers forward writes to the leader.

## Docs

- [`docs/API.md`](docs/API.md): the gRPC API (services, auth, status codes, gRPC-Web, load balancers).
- [`docs/PERSISTENCE.md`](docs/PERSISTENCE.md): how storage works, and how to make Redis durable enough for a vault.
- [`docs/LOGIN.md`](docs/LOGIN.md): sign-in with username, password and captcha; US (NIST) and China (等保 2.0) policy profiles; captcha providers for both regions.
- [`docs/LOGGING.md`](docs/LOGGING.md): operational logs (rotating JSON files) and the fail-closed, hash-chained audit log (file / syslog), including `audit-verify`.
- [`docs/AUTO-UNSEAL.md`](docs/AUTO-UNSEAL.md): auto-unseal via Vault/OpenBao Transit, AWS KMS or a static key, including migrating from Shamir.
- [`docs/SCALING.md`](docs/SCALING.md): running N replicas on plain Docker, Swarm or Kubernetes, with concurrency, unseal-all, seal-all and routing.
- [`docs/REDIS.md`](docs/REDIS.md): Redis standalone, Sentinel and Cluster, with key layout, WAIT durability and failover.
- [`docs/RAFT.md`](docs/RAFT.md): integrated Raft covering the join protocol, operations, and deploying on Kubernetes and on VMs.

## Stack

| Layer | Tech |
|-------|------|
| Backend | Rust · axum 0.7 · tokio · aes-gcm · sharks (Shamir) · redis-rs 0.27 · openraft 0.9 + redb 2 · auto-unseal: Transit / AWS KMS (SigV4) / static |
| Frontend | Svelte 5 (runes) · Vite 6 · TypeScript · @lucide/svelte |
| Storage | Redis 7 standalone / Sentinel / Cluster **or** integrated Raft |
| Deploy | Docker · compose (scaled) · Docker Swarm stacks · Helm (Deployment/StatefulSet) · systemd · HAProxy |
