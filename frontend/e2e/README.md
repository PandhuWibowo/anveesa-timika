# End-to-end scenarios

`make e2e` (or `cd frontend && bun e2e/run.ts [E | F07 …]`) runs 200 scenarios
against real processes: fresh single-node and 3-node Raft vaults on random ports,
a small SSH server (`backend/examples/ssh_target.rs`) for the bastion, and every
client the product has — gRPC-Web (the UI's transport), native gRPC over HTTP/2,
plain HTTP, the terminal WebSocket and the `timika operator` CLI.

| File | Area |
|---|---|
| `scenarios/a_protocol.ts` | HTTP health codes, grpc.health, gRPC-Web vs native, CORS, SPA, malformed input |
| `scenarios/b_lifecycle.ts` | init / unseal / seal / restart / rotate |
| `scenarios/c_auth.ts` | sign-in, captcha, lockout, sessions, password changes, rate limits |
| `scenarios/d_users.ts` | user administration |
| `scenarios/e_kv.ts` | KV versions, CAS, paths, listing, deletes, limits, concurrency, encryption at rest |
| `scenarios/f_bastion.ts` | servers, host-key pinning, grants, web terminal, recordings, kill |
| `scenarios/g_cluster.ts` | Raft join, standby health, linearizable follower reads, failover |
| `scenarios/h_audit_cli.ts` | audit log, hash chain, fail-closed, CLI, TLS |

Each run writes `report.md` / `report.json` to its work dir (`$E2E_DIR`, default a
temp dir); a full run also updates `REPORT.md` here.
