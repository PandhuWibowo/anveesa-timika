# Scaling out: many timika instances

timika runs as N replicas behind one endpoint on plain Docker, Docker Swarm,
or Kubernetes. The model depends on the storage backend:

| | `STORAGE=redis` | `STORAGE=raft` |
|---|---|---|
| Replicas are | **stateless**: all share one Redis | **stateful**: each keeps its own copy |
| Scale by | adding replicas freely (`--scale`, `docker service scale`, `replicas:`) | adding Raft members, 3 or 5 voters |
| Who serves writes | every replica | any node; followers forward to the leader |
| Workload kind | compose service / Swarm service / Deployment | one service per member / StatefulSet |

## What makes it safe to run N copies

**1. Optimistic concurrency.** Every read-modify-write (KV versions, key
rotation, init) commits with a guard: "the key I read must still hold exactly
this blob". The check is atomic with the write, inside the Redis Lua script or
the Raft state machine. If another replica got there first, the commit is
refused and retried on fresh data. With N replicas nothing is lost or
duplicated. Tested: 80 parallel writes to one secret through 2 replicas gave
exactly 80 versions, and concurrent `init` on 2 replicas produced exactly one winner.

**2. Every replica is sealed on its own.** The root key only ever lives in
memory, so each replica starts sealed and needs the unseal keys. Three tools
make that workable:

| Tool | Use |
|---|---|
| `timika operator unseal --all` | Run once per key inside any one replica. It finds every instance (Redis: the instance registry; Raft: members plus nodes waiting to join) and sends the key to each. |
| UI unseal screen | "Send this key to all N instances" is ticked by default when more than one instance exists. |
| `SysService/Unseal {"key":…,"all":true}` | The same fan-out, over the API. |

**3. Seal-all.** In an emergency, `timika operator seal --all` (or
`SysService/Seal {"all":true}`) seals **every** replica:
- **Redis:** it writes a marker signed with a key derived from the root key.
  Every unsealed replica checks it on its 3-second heartbeat and seals. A
  forged marker (bad signature) is ignored; this is tested.
- **Raft:** it sends a signed `/raft/seal` to every peer.

**4. Seal-aware routing.** A sealed replica is *alive* but must get no traffic:

| Check | Endpoint | Used for |
|---|---|---|
| Liveness | `/v1/sys/health?standbyok=true&sealedok=true&uninitok=true` | Docker `HEALTHCHECK`, k8s `livenessProbe`: never kill a sealed replica |
| Readiness | `/v1/sys/health?standbyok=true` (200 only when unsealed) | load balancer pool, k8s `readinessProbe` |

The image's `HEALTHCHECK` is liveness-only on purpose. If it failed while
sealed, Swarm and compose would restart sealed replicas forever.

## Instance registry

`SysService/Instances` (public, like `GetSealStatus`) and `timika operator
instances` list every replica with its seal state and address.

- **Redis:** each replica writes a heartbeat every 3s to `{prefix}#instances`
  (plaintext: id, address, sealed, version, no secrets). It shows as
  *unreachable* after 15s and is removed after 30s.
- **Raft:** the Raft membership, each member probed live, plus configured
  `retry_join` nodes that haven't joined yet.

Each replica advertises its own address. When `BIND_ADDR` is `0.0.0.0` and
`API_ADDR` isn't set, the container or pod IP is detected automatically.

## The bastion across replicas

- **Terminals** live on the replica that holds the WebSocket. Session records,
  recordings and command logs go to shared storage, so every replica lists and
  replays them. **End session** sets a flag on the stored session; the owning
  replica sees it within ~2 s, whichever replica you clicked on. A `live`
  session whose replica stops heartbeating shows as `lost`.
- **Files (SFTP)**: each replica keeps its own short-lived connection pool
  (one per user · server · account, closed after 2 idle minutes). Download
  links are signed with a key derived from the root key, so a link made on one
  replica works on any other — no sticky sessions needed.
- **Uploads and terminals** are long-lived HTTP / WebSocket requests: give the
  load balancer generous idle timeouts (`timeout tunnel` in HAProxy).
- Restarting or sealing a replica drops the terminals it was serving; people
  press **Reconnect**, and the UI retries reads on its own.

## Monitoring across replicas

One replica collects (Raft: the leader; Redis: the holder of `monitor/lease`,
taken over within three intervals); all serve the stored numbers. See
[MONITORING.md](MONITORING.md#how-it-scales).

## Infrastructure runs across replicas

- Each replica keeps its own bare clone of a repository (`AUTOMATION_DIR`) and
  fetches on demand, so a run started anywhere finds its commit.
- A run executes on the replica that started it, with a heartbeat. Output,
  plans and state go to shared storage, so **WatchRun** follows a run from any
  replica, and **Cancel** sets a flag that the running replica checks every
  second. If that replica dies, the run shows as `lost` after 30 s and can be
  closed with Cancel.
- Webhooks can hit any replica: the delivery is verified there and its runs
  start there.

---

## Plain Docker (multi-container)

`deploy/scale/docker-compose.yml` runs Redis, N timika replicas, and HAProxy.

```bash
docker compose -f deploy/scale/docker-compose.yml up -d --build --scale timika=3
docker compose -f deploy/scale/docker-compose.yml exec timika /app/timika operator init
docker compose -f deploy/scale/docker-compose.yml exec timika /app/timika operator unseal --all  # per key
# → http://localhost:8200
docker compose -f deploy/scale/docker-compose.yml up -d --scale timika=5   # then unseal --all again
```

`deploy/scale/haproxy.cfg` is the seal-aware LB. It keeps two pools over the
same replicas, discovered through Docker DNS:
- **ready** (unsealed only) takes all data traffic.
- **live** (any running replica) serves the UI and the seal-lifecycle
  endpoints, so you can init and unseal while everything is sealed.

It also retries on another replica when a connection fails, so a replica that
just died doesn't produce errors before its health check notices.

**Tested:** writes spread evenly over 4 replicas. Scaling 4 → 6 served 20 of 20
reads while the new replicas were sealed. Two hard kills of a replica each
served 30 of 30 reads immediately afterwards.

## Docker Swarm

| Stack | What |
|---|---|
| `deploy/swarm/stack.redis.yml` | Redis + a scalable timika service + 2 HAProxy replicas (`tasks.timika` discovery) |
| `deploy/swarm/stack.raft.yml` | 3 Raft members (one single-replica service each, own volume, stable hostname) + HAProxy |

```bash
docker stack deploy -c deploy/swarm/stack.redis.yml timika
docker exec -it $(docker ps -qf name=timika_timika | head -1) /app/timika operator init
docker exec -it $(docker ps -qf name=timika_timika | head -1) /app/timika operator unseal --all
docker service scale timika_timika=5
```

- **Image distribution:** on a multi-node swarm, push the image to a registry
  and set `TIMIKA_IMAGE`.
- **Redis stack:** the bundled Redis is a single node pinned to the manager.
  For production, point `REDIS_*` at Sentinel or Cluster (docs/REDIS.md).
- **Rolling updates** use `order: start-first, parallelism: 1`. Each
  replacement starts sealed, so unseal as tasks roll.

**Tested on a single-node swarm:**
- **Redis stack:** unsealed 3 tasks with one command, wrote through the LB, then
  scaled to 5 with no errors while the new tasks were sealed.
- **Raft stack:** `instances` showed the 2 joining nodes, one `unseal --all`
  joined them as voters, and `seal --all` sealed all 3 members.

## Kubernetes

The Helm chart supports both backends:

```bash
# stateless replicas on Redis (Deployment; scale freely)
helm install timika deploy/helm/timika --set backend=redis,replicas=4 \
  --set redis.mode=sentinel,redis.sentinels=sentinel-0:26379\,sentinel-1:26379\,sentinel-2:26379,redis.waitReplicas=1

# integrated Raft (StatefulSet; 3 or 5)
helm install timika deploy/helm/timika --set backend=raft,replicas=3
```

- **Redis mode** creates a Deployment with `maxUnavailable: 0`. A rollout never
  removes a serving pod before its replacement is unsealed, so it pauses until
  you run `kubectl exec deploy/timika-timika -- /app/timika operator unseal --all`.
- **Readiness means unsealed**, so the Service only routes to unsealed pods.
  Liveness never depends on seal state.

The chart is rendered and linted for both modes; it hasn't been deployed to a
live cluster yet.

## Sealed replicas and auto-unseal

With the default Shamir seal, every new replica (scale-up, rollout,
crash-restart) starts **sealed**. Seal-aware routing keeps that safe, but the
added capacity only arrives once someone runs `unseal --all`.

With **auto-unseal** ([AUTO-UNSEAL.md](AUTO-UNSEAL.md)), replicas unwrap the
root key through a key service (Vault/OpenBao Transit, AWS KMS, or a static key
secret) and serve within seconds. Scaling becomes fully automatic, and
autoscalers (HPA, Swarm autoscalers) become practical. Tested with Transit:
3 → 6 replicas were all unsealed 6s after scaling, and after restarting every
replica the service was back in 10s. Seal-all still pauses auto-unseal until
restart, so the emergency brake keeps working.
