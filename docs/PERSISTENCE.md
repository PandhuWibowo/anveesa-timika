# Persistence — how a vault stores data without "a database"

**Short answer:** HashiCorp Vault has no database of its own. It has a *storage
backend*: a dumb key/value store (Raft, Consul, S3, a file, Postgres…) that only
ever receives **ciphertext**. All the intelligence — encryption, versioning, auth,
policies — lives in the Vault process. timika copies that model with two
storage backends, chosen by `STORAGE`:

- **`redis`**: Redis *is* the database, and the fastest option. It works as
  standalone, with Sentinel failover, or on Redis Cluster; see [REDIS.md](REDIS.md).
- **`raft`**: integrated storage. Each timika node keeps its own copy in an
  embedded redb file and replicates through Raft. It's HA with no external
  database; see [RAFT.md](RAFT.md).

Either way, there is no second database.

## The layers

```
 gRPC / gRPC-Web  timika.v1.*          ← API (tonic on axum)
   │
 logical engines  (kv.rs, token.rs)    ← know what a "secret" or "token" is
   │  plaintext bytes + a storage path
 barrier          (barrier.rs, core.rs)← AES-256-GCM, path bound as AAD
   │  ciphertext bytes + the same path
 physical         (storage/)           ← get · commit(puts, deletes) · list · ping
   │
   ├─ redis:  Redis ── AOF + RDB ──▶ disk
   └─ raft:   Raft log ──majority fsync──▶ redb file on every node
```

The physical layer has a handful of operations and no idea what it is storing.
Both backends implement the same enum in `storage/mod.rs`. Adding another
(etcd, S3…) touches nothing above it.

## Key hierarchy

```
 unseal shares (5, held by people) ──Shamir 3-of-5──▶ root key   [memory only]
                                                         │ AES-GCM
                                                         ▼
                                          core/keyring   [Redis, ciphertext]
                                            term 1 key, term 2 key, …
                                                         │ AES-GCM (active term)
                                                         ▼
                              kv/*, sys/*   [Redis, ciphertext]
```

- The **root key is never written anywhere.** It is rebuilt from shares at unseal
  and dropped at seal or process exit.
- The **keyring** is the only thing the root key encrypts. Rotating a data key
  (`SysService/Rotate`) adds a term; old blobs stay readable because every blob
  records its term in a 5-byte header.
- The only plaintext record is `core/seal-config` (share count and threshold).

## Lifecycle

| Step | What happens | Redis |
|------|--------------|-------|
| `POST /sys/init` | Generate root key + keyring + root token, split root key into shares | 3 keys written in one MULTI |
| (process start) | Always **sealed** — nothing can be decrypted | read `core/seal-config` only |
| `POST /sys/unseal` ×T | Collect T shares → rebuild root key → decrypt keyring | read `core/keyring` |
| `PUT /kv/data/p` | Encrypt → MULTI { SET ver, SET meta, DEL pruned } | write |
| `POST /sys/seal` | Zero the keys in memory | nothing |

## Making Redis durable (STORAGE=redis — this is the part that matters)

With Raft none of this applies: every write is fsynced by a majority of nodes
before it's acknowledged.

Redis defaults are tuned for caches, not for the only copy of your secrets:

| Setting | Use | Why |
|---------|-----|-----|
| `appendonly yes` | required | Every write is logged and replayed on restart. |
| `appendfsync everysec` | recommended | ≤1s of writes lost on power failure. `always` loses none, at a throughput cost. |
| `maxmemory-policy noeviction` | **required** | Any LRU/LFU/TTL policy lets Redis *delete keys* when memory fills — including `core/keyring`, which would destroy the whole vault. With `noeviction`, writes fail loudly instead. |
| RDB `save` | recommended | Snapshots are the easiest thing to back up. |

`make redis` and `docker-compose.yml` start Redis with these flags. The backend
warns at boot if they're wrong, and `SysService/Storage` shows them live.

## Backups

Back up the RDB/AOF files like any database. A backup is useless without the
unseal keys, which is the point: it can live somewhere less trusted. **Also keep
the unseal keys**, separately. Losing them makes every backup unreadable forever.

## Known limits of this base

- **Many timika instances per Redis are fine.** Read-modify-writes commit with
  compare-and-swap guards (see SCALING.md), so replicas never overwrite each other.
- **Redis replication is async** unless `REDIS_WAIT_REPLICAS` is set; see REDIS.md.
- **Paths are plaintext** in storage key names (same as Vault). Don't put secrets
  in paths.
- No audit log, policies, or token creation yet — root token only.
