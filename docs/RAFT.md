# Integrated Raft storage

`STORAGE=raft` makes timika its own HA database, the same idea as Vault's
[integrated storage](https://developer.hashicorp.com/vault/docs/configuration/storage/raft).
No Redis, no Postgres: every node keeps a full copy of the encrypted data on its
own disk, and a write is acknowledged only after a **majority** of nodes has
fsynced it.

```
            clients / k8s Service (ready = unsealed pods)
                 │            │             │
          ┌──────▼─────┐ ┌────▼──────┐ ┌────▼──────┐
          │ timika-0   │ │ timika-1  │ │ timika-2  │
          │ LEADER     │◀┤ follower  │ │ follower  │   followers forward writes
          │            │ └───────────┘ └───────────┘   to the leader
          │ :8200 api  │   ▲ raft RPC :8201 (HMAC-signed, optional TLS)
          │ :8201 raft ├───┴──────────────┘
          │ redb file  │  each node: logs + state machine in data/raft.redb
          └────────────┘
```

| Voters | Survives | Notes |
|--------|----------|-------|
| 1 | 0 failures | dev only |
| 3 | 1 failure | recommended default |
| 5 | 2 failures | large / multi-zone |

Always use an odd number: 4 voters survive no more failures than 3.

## How it works

- **Log + state machine.** A write becomes one Raft log entry: an atomic batch of
  puts and deletes, with values that are already ciphertext. Committed entries
  are applied to every node's redb `data` table in the same transaction as
  `last_applied`, so a restart never re-applies or loses anything.
- **Any node serves any call; the log decides.** Writes and membership changes
  made on a follower are forwarded to the leader over the signed cluster port.
  Reads are linearizable everywhere: the leader confirms leadership with a
  quorum (`ensure_linearizable`); a follower asks the leader for its read index
  and waits until it has applied that far, then reads locally.
- **Snapshots.** Every `RAFT_SNAPSHOT_THRESHOLD` entries the leader snapshots the
  key space and trims the log, keeping `RAFT_TRAILING_LOGS` entries. A node too
  far behind gets the whole snapshot in one request.
- **Sealed nodes don't take part.** Raft starts at unseal and stops at seal.
  Peers authenticate every RPC with an HMAC key derived from the **root key**, so
  only a node unsealed with the cluster's keys can take part.

## Joining: how a new node proves it belongs

1. A new node with `RAFT_RETRY_JOIN` keeps asking those addresses for a
   **challenge**. The leader returns the seal config, the root-key-encrypted
   keyring, and a random nonce encrypted by the barrier.
2. The node now shows as *initialized · sealed · joining*. An operator unseals
   it with the **same** unseal keys, which rebuilds the root key and decrypts
   the keyring and the nonce.
3. The node sends the nonce back. The leader checks it (one attempt, 5-minute
   TTL), adds the node as a learner, replicates everything to it, and promotes
   it to voter.

Nobody gets in without the unseal keys, and there is no shared cluster secret to
distribute.

## Operating

| Task | How |
|------|-----|
| Cluster members | `ClusterService/Configuration` or `timika operator instances` |
| Remove a dead node | `ClusterService/RemovePeer {"node_id":"timika-2"}` |
| Backup | `ClusterService/Snapshot` (streamed ciphertext, safe to store off-site) |
| Health for LBs/probes | `GET /v1/sys/health`: 200 leader · 429 follower · 503 sealed · 501 uninit; `?standbyok=true` etc. |
| Node restarted | comes back sealed; unseal it and it catches up on its own |
| Wiped disk | remove it as a peer, restart it empty, and unseal it; it re-joins with a snapshot |

## Configuration

| Env | Default | |
|-----|---------|--|
| `STORAGE` | `redis` | set to `raft` |
| `RAFT_NODE_ID` | `$HOSTNAME` | unique and **stable**; the node's identity |
| `RAFT_PATH` | `./data/raft` | holds `raft.redb` |
| `BIND_ADDR` / `CLUSTER_BIND_ADDR` | `127.0.0.1:8200` / `:8201` | listeners |
| `API_ADDR` / `CLUSTER_ADDR` | derived from binds | how **peers** reach this node; set explicitly in real deployments |
| `RAFT_RETRY_JOIN` | – | comma-separated peer API addresses (may include itself) |
| `RAFT_SNAPSHOT_THRESHOLD` | `8192` | |
| `RAFT_TRAILING_LOGS` | `10000` | |
| `TLS_CERT_FILE` / `TLS_KEY_FILE` | – | serve both listeners over TLS |
| `TLS_CA_FILE` | – | CA used to verify peers |

---

## Deploying on Kubernetes

The Helm chart in `deploy/helm/timika` creates:

- **StatefulSet**: one PVC per pod, `podManagementPolicy: Parallel`, soft
  anti-affinity across nodes, and a non-root user.
- **Headless service `<rel>-timika-internal`**: stable pod DNS names for
  `API_ADDR`, `CLUSTER_ADDR` and `retry_join`. It uses
  `publishNotReadyAddresses: true`, so sealed pods can still find each other.
- **Client service `<rel>-timika`**: only includes *ready* pods. Readiness means
  unsealed (`/v1/sys/health?standbyok=true`), so clients never hit a sealed node.
- **PodDisruptionBudget**: `maxUnavailable: 1`, so drains and upgrades never
  break quorum.

```bash
helm install timika deploy/helm/timika -n timika --create-namespace \
  --set image.repository=<registry>/anveesa-timika --set image.tag=0.2.0

kubectl -n timika port-forward pod/timika-timika-0 8200:8200 &
timika operator init --addr http://localhost:8200 --shares 5 --threshold 3
# unseal timika-timika-0 with 3 keys, then the same for -1 and -2 (port-forward each)
```

**TLS** (recommended): use cert-manager to issue a certificate whose SANs cover
`*.timika-timika-internal.timika.svc` and `timika-timika.timika.svc`. Store it
in a secret with `tls.crt`, `tls.key` and `ca.crt`, then set
`--set tls.enabled=true,tls.secretName=<secret>`.

Plain manifests without Helm: `make helm-template > timika.yaml`.

## Deploying without Kubernetes (VMs / bare metal)

Three machines, each running the same binary with its own identity. For example,
`/etc/timika/timika.env` on `10.0.0.1`:

```bash
STORAGE=raft
RAFT_NODE_ID=timika-1
RAFT_PATH=/var/lib/timika
BIND_ADDR=0.0.0.0:8200
CLUSTER_BIND_ADDR=0.0.0.0:8201
API_ADDR=https://10.0.0.1:8200
CLUSTER_ADDR=https://10.0.0.1:8201
RAFT_RETRY_JOIN=https://10.0.0.1:8200,https://10.0.0.2:8200,https://10.0.0.3:8200
TLS_CERT_FILE=/etc/timika/tls/tls.crt
TLS_KEY_FILE=/etc/timika/tls/tls.key
TLS_CA_FILE=/etc/timika/tls/ca.crt
```

`/etc/systemd/system/timika.service`:

```ini
[Unit]
Description=anveesa-timika secrets manager
After=network-online.target
Wants=network-online.target

[Service]
User=timika
EnvironmentFile=/etc/timika/timika.env
ExecStart=/usr/local/bin/timika
Restart=on-failure
LimitNOFILE=65536
# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/timika
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

Open ports 8200 (API) and 8201 (cluster) between the nodes. Put a load balancer
in front of 8200 that health-checks `/v1/sys/health?standbyok=true`. Then
initialize one node and unseal all three, exactly as on Kubernetes.

With Docker instead: `docker-compose.raft.yml` runs the same 3-node layout on one
host (`make raft-up`).

## Local development

```bash
make raft-dev    # 3 nodes on :8200/:8210/:8220 (RESET=1 / make raft-reset wipes data)
make frontend    # UI on :5174 → timika-0
```
