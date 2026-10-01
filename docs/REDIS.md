# Redis storage — standalone, Sentinel, Cluster

`STORAGE=redis` stores the vault in Redis. It's the fast path: a read is one or
two `GET`s against memory. Pick the topology with `REDIS_MODE`.

| `REDIS_MODE` | Use when | Failover | Config |
|---|---|---|---|
| `standalone` (default) | dev, single server, managed Redis with its own HA endpoint | none (or the provider's) | `REDIS_URL` |
| `sentinel` | self-hosted HA: 1 primary + replicas + 3 sentinels | Sentinel promotes a replica; timika follows | `REDIS_SENTINELS`, `REDIS_SENTINEL_MASTER` |
| `cluster` | an existing Redis Cluster, or a provider that only offers cluster mode | the cluster promotes the replica | `REDIS_CLUSTER_NODES` |

## Key layout (the same in every mode)

```
{timika}:core/seal-config      plaintext share counts (the only plaintext)
{timika}:core/keyring          keyring, encrypted with the root key
{timika}:sys/token/<sha256>    token entries        ─┐
{timika}:kv/meta/<path>        secret metadata       ├ barrier-encrypted
{timika}:kv/ver/<path>@<n>     secret versions      ─┘
{timika}#index                 sorted set of every path (listing)
```

- **`{timika}` is a Cluster hash tag.** Every key lands in the same slot
  (15460 for the default prefix), so one shard holds the whole vault. That
  keeps multi-key commits atomic. Vault data is small, so sharding it would buy
  nothing, and your other applications can still use the other slots.
- **Commits are one Lua script:** the SETs and DELs plus the index update,
  atomic in all three modes.
- **Listing uses `ZRANGEBYLEX`** on the index: O(log n), with no `SCAN` over
  the keyspace.
- **Upgrades migrate automatically.** Data from the old layout (`timika:<path>`)
  is moved and indexed on startup (standalone and Sentinel).

## Durability: `REDIS_WAIT_REPLICAS`

Redis replication is **asynchronous**. Without `WAIT`, the primary acknowledges
a write before any replica has it. If it dies right then, the promoted replica
won't have that write: an acknowledged secret is lost.

With `REDIS_WAIT_REPLICAS=N`, timika sends `WAIT N` after each commit and only
returns success once N replicas confirm. If they don't confirm within
`REDIS_WAIT_TIMEOUT_MS`, the request fails with **503**. The write did reach the
primary, but it isn't guaranteed to survive a failover. The client can retry,
which creates a new version.

The trade-off, measured in the test rigs:

| Setup | Primary dies → | Writes during the outage |
|---|---|---|
| Sentinel, 2 replicas, `WAIT 1` | new primary in **~6–7s** | resume, still confirmed by the remaining replica |
| Cluster, 1 replica per shard, `WAIT 1` | new primary in **~7s** | **refused (503)** until that shard has a replica again, because nothing is left to confirm |
| any, `WAIT 0` | same failover time | accepted, but a write acknowledged just before the failure can be lost |

**Recommendation:** use `REDIS_WAIT_REPLICAS=1` with **at least 2 replicas** on
the primary that holds timika's data. For Cluster, that means 2 replicas for
the shard owning timika's slot.

Also set on every Redis node: `appendonly yes`, `appendfsync everysec` and
`maxmemory-policy noeviction`. `SysService/Storage` reports all of these live.

## Configuration

| Env | Default | |
|-----|---------|--|
| `REDIS_MODE` | `standalone` | `standalone` · `sentinel` · `cluster` |
| `REDIS_URL` | `redis://127.0.0.1:6380` | standalone; `rediss://` for TLS; auth in the URL |
| `REDIS_SENTINELS` | – | `host:26379,host:26379,…` (or full URLs with auth) |
| `REDIS_SENTINEL_MASTER` | `mymaster` | the monitored primary's name |
| `REDIS_CLUSTER_NODES` | – | seed nodes `host:6379,…` (`rediss://…` for TLS) |
| `REDIS_USERNAME` / `REDIS_PASSWORD` | – | data-node ACL credentials (Sentinel and Cluster) |
| `REDIS_TLS` | `false` | Sentinel: use TLS to the data nodes |
| `REDIS_WAIT_REPLICAS` | `0` | replicas that must confirm each write |
| `REDIS_WAIT_TIMEOUT_MS` | `1000` | |
| `REDIS_TIMEOUT_MS` | `3000` | connect and response timeout; how fast a dead primary is noticed |
| `TIMIKA_PREFIX` | `timika` | key namespace and hash tag |

## How failover is handled

- **Sentinel.** On a connection error or `READONLY` (the primary was demoted),
  timika asks Sentinel for the current primary and retries the request once. A
  watcher also checks the node's role every 2s, so a failover is noticed even
  when nothing is being written. Each discovery attempt is time-capped: during a
  failover, Sentinel can still name the dead primary for a few seconds.
- **Cluster.** The slot-aware client follows `MOVED` and refreshes the slot map
  by itself. Commands without keys (`WAIT`, `INFO`, `CONFIG`) are routed to the
  primary that owns timika's slot, not to a random node.

## Test rigs

```bash
make redis                 # standalone on :6380
make redis-sentinel-up     # primary + 2 replicas + 3 sentinels + timika on :8200
make redis-cluster-up      # 6-node cluster + timika on :8200
```

In both rigs timika runs inside the Docker network. Sentinel and Cluster hand
out container IPs, which a macOS or Windows host can't reach. To try a failover,
stop the primary (`docker compose -f deploy/redis/docker-compose.sentinel.yml stop redis-primary`).
