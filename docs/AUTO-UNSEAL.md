# Auto-unseal

With Shamir (the default), a person must enter unseal keys on **every** instance
after **every** start. With auto-unseal, the root key is **wrapped** (encrypted)
by an external key service and stored next to the data. Each instance unwraps it
at boot and unseals itself, so scaling out, rolling updates and crash-restarts
need no human.

```
 init:   root key ──wrap (key service)──▶ core/root-key-wrapped   (stored with the data)
 boot:   core/root-key-wrapped ──unwrap (key service)──▶ root key (memory) ──▶ keyring ──▶ data
```

**What changes in trust:** anyone who can call the key service's *decrypt* with
timika's credentials *and* read the storage can unseal. Protect those
credentials like unseal keys. If the key-service key is **deleted or
permanently disabled, the vault is unrecoverable**; that's the equivalent of
losing the Shamir shares. Disabling it temporarily is a valid way to lock the
vault.

## Key services

| `SEAL_TYPE` | Key service | Settings |
|---|---|---|
| `shamir` | none (default) | – |
| `transit` | HashiCorp Vault / OpenBao Transit | `SEAL_TRANSIT_ADDR`, `SEAL_TRANSIT_KEY`, `SEAL_TRANSIT_MOUNT` (`transit`), `SEAL_TRANSIT_TOKEN` or `SEAL_TRANSIT_TOKEN_FILE`, `SEAL_TRANSIT_NAMESPACE`, `SEAL_TRANSIT_CA_FILE` |
| `awskms` | AWS KMS | `SEAL_AWSKMS_KEY_ID` (ARN or `alias/…`), `SEAL_AWSKMS_REGION`, `SEAL_AWSKMS_ENDPOINT` (optional) |
| `static` | a 32-byte key in a file or env var | `SEAL_STATIC_KEY_FILE` or `SEAL_STATIC_KEY` (base64 or hex) |

### Transit (Vault / OpenBao)

Give timika a token that can do **only** encrypt and decrypt with one key:

```hcl
path "transit/encrypt/timika-unseal" { capabilities = ["update"] }
path "transit/decrypt/timika-unseal" { capabilities = ["update"] }
```

- `SEAL_TRANSIT_TOKEN_FILE` is re-read on every call, so tokens rotated by an
  agent or sidecar are picked up.
- Rotating the Transit key is safe: older ciphertext versions keep decrypting.
  This was tested.

### AWS KMS

The IAM policy needs `kms:Encrypt` and `kms:Decrypt` on the key. Credentials
are resolved in the usual AWS order:
1. `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (plus `AWS_SESSION_TOKEN`)
2. **EKS IRSA** (web identity): in Helm, set `serviceAccount.create=true` and the
   `eks.amazonaws.com/role-arn` annotation
3. ECS task role
4. EC2 instance profile (IMDSv2)

### Static key: lowest assurance

For platforms without a KMS. The key lives in a Kubernetes or Swarm secret:

```bash
head -c 32 /dev/urandom | base64 | docker secret create timika_seal_key -     # Swarm
kubectl create secret generic timika-seal --from-literal=key="$(head -c 32 /dev/urandom | base64)"  # k8s
```

It still keeps the root key out of the storage, and out of Redis/Raft
snapshots and backups. But whoever holds both the secret and a storage backup
can decrypt everything. Keep them apart, and prefer `transit` or `awskms`
where you can.

## Behavior

| Situation | What happens |
|---|---|
| `init` | The root key is wrapped **before** anything is written; the vault is unsealed immediately. **No unseal keys** are issued, only the root token. |
| Instance starts, restarts, or is added by scaling | It unwraps the root key and is serving within seconds. |
| Key service unreachable at boot | The instance stays sealed, shows the error (UI, `seal-status`), and **retries every 5s**. It recovers by itself. |
| Wrong key, or key disabled or deleted | It stays sealed with the key service's error (e.g. `DisabledException`). |
| `seal` / **seal-all** | Those instances stay sealed; auto-unseal is **paused until they restart**, so an emergency seal isn't undone seconds later. To resume: `docker compose restart`, `docker service update --force`, `kubectl rollout restart`. |
| Raft: new member | The wrapped key travels with the join challenge, so it unwraps, proves membership and joins as a voter with no operator. |
| Manual `unseal` with keys | Refused; instances unseal themselves. |

## Migrating an existing Shamir vault

1. Restart the instances with `SEAL_TYPE` and the key-service settings.
   They report "vault still uses Shamir keys".
2. Unseal **one** instance with the Shamir shares, as usual.
3. It wraps the root key, switches the vault to the new seal type, and logs
   `seal migrated: shamir → …`. The other instances auto-unseal on their next
   attempt. The Shamir shares are no longer used.

Migrating back to Shamir, or between two key services, isn't supported yet.

## What was tested

| Key service | Tested against | Result |
|---|---|---|
| Transit | OpenBao dev server, least-privilege token | init, restart, key rotation, outage then self-recovery |
| AWS KMS | LocalStack | init, restart, disabled key (stays sealed), re-enabled key (recovers) |
| static | – | init, restart, wrong key, second replica |

- **AWS signing:** LocalStack doesn't verify request signatures. The SigV4
  signer is checked separately against AWS's official `get-vanilla` test vector.
- **Not yet exercised against real AWS:** the IRSA, ECS and EC2 credential paths.
- **Scale-out with Transit (Docker):** 3 → 6 replicas were all unsealed 6s after
  scaling; after restarting every replica, the service was serving again in 10s;
  seal-all held.
- **Raft with auto-unseal:** `init` on one node gives a 3-voter cluster in about
  10s, with no unseal commands anywhere.
