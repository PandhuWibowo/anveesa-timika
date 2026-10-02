# End-to-end scenarios

`make e2e` (or `cd frontend && bun e2e/run.ts [E | F07 …]`) runs 314 scenarios
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
| `scenarios/i_people_audit.ts` | roles (`ssh`), one-step user + grants, account provisioning (sudo, groups), command log, audit trail filters |
| `scenarios/j_files.ts` | SFTP browse / upload / download links / rename / delete, archives (compress, extract, stream), hostile names |
| `scenarios/k_mfa.ts` | TOTP setup, sign-in challenge, replay, recovery codes, required 2FA, admin reset |
| `scenarios/m_monitor.ts` | agentless monitoring: readings and rates, history ranges, down detection, alert rules + notifications, access, cleanup, restart, containers (history, logs, actions) |
| `scenarios/n_containers.ts` | Docker and Podman: container details, files in a container (browse, upload, download, folders), images, volumes, admin-only access |
| `scenarios/l_automation.ts` | Git repositories, project detection, runs on a runner (real terraform / ansible when installed), plan → apply once, state in the vault, masking, webhooks, cancel, audit, run options (workspaces, destroy, Ansible on timika's servers), approvals, schedules, notifications, CI trigger, file viewer |

Each run writes `report.md` / `report.json` to its work dir (`$E2E_DIR`, default a
temp dir); a full run also updates `REPORT.md` here.
