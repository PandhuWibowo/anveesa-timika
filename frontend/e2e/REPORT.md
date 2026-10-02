# timika e2e report

314/314 passed · 2026-10-02T07:16:23.558Z

| Category | Passed |
|---|---|
| A · HTTP & protocol | 20/20 |
| B · Init, unseal, seal | 25/25 |
| C · Sign-in & sessions | 38/38 |
| D · User administration | 15/15 |
| E · KV secrets | 42/42 |
| F · Bastion | 48/48 |
| G · Raft cluster | 10/10 |
| H · Audit log & CLI | 13/13 |
| I · People, roles & audit trail | 22/22 |
| J · Files (SFTP) | 24/24 |
| K · Two-factor (MFA) | 15/15 |
| L · Infrastructure automation | 26/26 |
| M · Monitoring | 9/9 |
| N · Containers (Docker / Podman) | 7/7 |

| ID | Scenario | Result | ms |
|---|---|---|---|
| A01 | health is 501 on an uninitialized node | pass | 1875 |
| A02 | health ?uninitok=true turns 501 into 200 | pass | 1 |
| A03 | health is 503 while sealed | pass | 204 |
| A04 | health ?sealedok=true turns 503 into 200 | pass | 1 |
| A05 | health is 200 on the unsealed leader, with engine and node | pass | 1611 |
| A06 | health reports a semver version | pass | 1 |
| A07 | grpc.health.v1 says NOT_SERVING while sealed | pass | 14 |
| A08 | grpc.health.v1 says SERVING once unsealed | pass | 479 |
| A09 | grpc.health.v1 answers over gRPC-Web too (browser-based probes) | pass | 3 |
| A10 | native gRPC (HTTP/2) and gRPC-Web return the same seal status | pass | 8 |
| A11 | an unknown method returns UNIMPLEMENTED | pass | 3 |
| A12 | an unknown service returns UNIMPLEMENTED, not the SPA | pass | 4 |
| A13 | a malformed protobuf body is rejected and the server stays up | pass | 3 |
| A14 | CORS preflight for gRPC-Web is allowed | pass | 1 |
| A15 | CORS exposes the grpc-status header to browsers | pass | 1 |
| A16 | the SPA is served for / and for client-side routes | pass | 3 |
| A17 | an unknown /v1 path is a 404, not the SPA | pass | 1 |
| A18 | 200 concurrent gRPC-Web calls all succeed | pass | 93 |
| A19 | responses carry an x-timika-request-id for correlation with the audit log | pass | 1 |
| A20 | a call to a sealed node fails with FAILED_PRECONDITION "vault is sealed" | pass | 1 |
| B01 | status of a new node: uninitialized, sealed, raft, node name | pass | 160 |
| B02 | init rejects threshold > shares | pass | 160 |
| B03 | init rejects zero shares | pass | 162 |
| B04 | init rejects more than 16 shares | pass | 161 |
| B05 | init rejects share counts that overflow (300) | pass | 163 |
| B06 | init returns N distinct keys and a tmk. root token, and leaves the vault sealed | pass | 194 |
| B07 | a 1-of-1 vault unseals with its single key | pass | 199 |
| B08 | init twice is refused | pass | 195 |
| B09 | unseal rejects a key that is not base64 | pass | 196 |
| B10 | unseal rejects base64 that is not a share | pass | 195 |
| B11 | one key moves progress to 1 of the threshold | pass | 194 |
| B12 | submitting the same key twice does not count twice | pass | 196 |
| B13 | reset clears unseal progress | pass | 196 |
| B14 | another vault's keys are rejected and progress resets | pass | 394 |
| B15 | unseal on an uninitialized node is FAILED_PRECONDITION | pass | 161 |
| B16 | unseal on an unsealed vault is a harmless no-op | pass | 201 |
| B17 | any threshold-sized subset of keys unseals (keys 2 and 3) | pass | 197 |
| B18 | seal without a token is UNAUTHENTICATED | pass | 198 |
| B19 | seal with a read-only token is PERMISSION_DENIED | pass | 688 |
| B20 | seal by root seals; data calls then fail with "vault is sealed" | pass | 205 |
| B21 | a sealed vault reports sealed, not "bad token", for token calls | pass | 202 |
| B22 | seal → unseal keeps every secret | pass | 225 |
| B23 | a restarted process comes back sealed; its data survives | pass | 384 |
| B24 | rotate adds a key term; old and new secrets stay readable | pass | 243 |
| B25 | a rotated keyring survives seal + unseal | pass | 253 |
| C01 | login config is public and names the policy profile and captcha | pass | 1 |
| C02 | the cn-mlps profile is reported with its own rules | pass | 161 |
| C03 | login config never exposes captcha secrets | pass | 161 |
| C04 | a captcha challenge is signed and expires | pass | 1 |
| C05 | no captcha challenges while sealed (the key is derived from the root key) | pass | 195 |
| C06 | login without a captcha is refused | pass | 1 |
| C07 | a captcha for a different provider is refused | pass | 1 |
| C08 | a wrong proof-of-work answer is refused | pass | 2 |
| C09 | a solved captcha cannot be replayed | pass | 270 |
| C10 | a self-made challenge with a forged signature is refused | pass | 1 |
| C11 | the system-use notice must be acknowledged (us-nist) | pass | 1 |
| C12 | a good sign-in returns a session token with expiry | pass | 244 |
| C13 | a wrong password is UNAUTHENTICATED with a generic message | pass | 235 |
| C14 | an unknown user gets exactly the same answer (no user enumeration) | pass | 458 |
| C15 | an impossible username gets the same generic answer | pass | 225 |
| C16 | usernames are case-insensitive at sign-in | pass | 237 |
| C17 | the account locks after the threshold of failures, even for the right password | pass | 1416 |
| C18 | an administrator can unlock a locked account | pass | 1522 |
| C19 | a successful sign-in resets the failure counter | pass | 1887 |
| C20 | a disabled user cannot sign in (generic message) | pass | 702 |
| C21 | disabling a user ends their live sessions immediately | pass | 473 |
| C22 | deleting a user ends their live sessions immediately | pass | 478 |
| C23 | a new user must change the admin-set password at first sign-in | pass | 466 |
| C24 | a change-password-only session cannot read secrets | pass | 466 |
| C25 | a change-password-only session can look itself up | pass | 479 |
| C26 | change password with the wrong current password is refused | pass | 228 |
| C27 | the new password must differ from the current one | pass | 442 |
| C28 | the new password must meet the policy | pass | 1 |
| C29 | changing the password ends the session; the new password grants full access | pass | 1379 |
| C30 | cn-mlps: a recently used password cannot be reused | pass | 2259 |
| C31 | the root token has no password to change | pass | 1 |
| C32 | lookup-self on the root token: root policy, no user, no expiry | pass | 1 |
| C33 | the root token is not renewable; a session is | pass | 1130 |
| C34 | revoke-self ends the session | pass | 279 |
| C35 | tokens work as `authorization: Bearer` too | pass | 1 |
| C36 | a made-up token is UNAUTHENTICATED | pass | 1 |
| C37 | sign-in attempts are rate limited per client address | pass | 899 |
| C38 | the second sign-in reports when and from where the previous one was | pass | 703 |
| D01 | admins list users and the assignable roles | pass | 3 |
| D02 | read-only users cannot list users | pass | 1 |
| D03 | an invalid username is refused | pass | 1 |
| D04 | a new user needs a password | pass | 1 |
| D05 | an unknown role is refused | pass | 1 |
| D06 | a weak password is refused when creating a user | pass | 1 |
| D07 | a password containing the username is refused | pass | 1 |
| D08 | get-user returns the profile and never a password hash | pass | 1 |
| D09 | get-user for an unknown name is NOT_FOUND | pass | 1 |
| D10 | changing only the roles keeps the password | pass | 476 |
| D11 | an administrator cannot delete their own account | pass | 1 |
| D12 | deleting an unknown user is NOT_FOUND | pass | 1 |
| D13 | usernames are stored lower-case | pass | 230 |
| D14 | read-only users cannot create users | pass | 1 |
| D15 | unlocking an unknown user is NOT_FOUND | pass | 1 |
| E01 | write then read a secret | pass | 10 |
| E02 | JSON types survive the round trip (numbers, bools, null, nested, arrays) | pass | 10 |
| E03 | unicode and emoji values survive | pass | 10 |
| E04 | versions count up with every write | pass | 27 |
| E05 | an older version can be read | pass | 26 |
| E06 | reading a secret that does not exist is NOT_FOUND | pass | 1 |
| E07 | reading a version that does not exist is NOT_FOUND | pass | 11 |
| E08 | cas=0 creates a new secret | pass | 8 |
| E09 | cas=0 refuses to overwrite an existing secret (ABORTED) | pass | 11 |
| E10 | cas=current succeeds | pass | 18 |
| E11 | a stale cas is ABORTED | pass | 20 |
| E12 | `..` in a path is refused | pass | 1 |
| E13 | an empty path is refused | pass | 2 |
| E14 | spaces in a path are refused | pass | 1 |
| E15 | leading and trailing slashes are ignored | pass | 11 |
| E16 | an empty segment (a//b) is refused | pass | 1 |
| E17 | non-ASCII path segments are refused | pass | 1 |
| E18 | a `.` segment is refused | pass | 1 |
| E19 | dots, dashes and underscores are fine in names | pass | 11 |
| E20 | listing a folder shows sub-folders with a trailing slash and secrets | pass | 18 |
| E21 | a listing shows only immediate children, once each | pass | 48 |
| E22 | the root listing includes top-level folders | pass | 12 |
| E23 | listing a folder that does not exist is NOT_FOUND | pass | 1 |
| E24 | soft delete with no versions deletes the latest | pass | 30 |
| E25 | soft delete of chosen versions leaves the others | pass | 43 |
| E26 | writing after a soft delete makes a new readable version | pass | 32 |
| E27 | destroy removes every version and the metadata | pass | 36 |
| E28 | destroying a secret that does not exist is NOT_FOUND | pass | 1 |
| E29 | old versions are pruned beyond KV_MAX_VERSIONS | pass | 268 |
| E30 | metadata carries versions and timestamps in order | pass | 28 |
| E31 | read-only users can read and list | pass | 15 |
| E32 | read-only users cannot write | pass | 2 |
| E33 | read-only users cannot delete or destroy | pass | 13 |
| E34 | every KV call needs a token | pass | 3 |
| E35 | a 1 MiB value round-trips | pass | 495 |
| E36 | an oversized (8 MiB) write is refused cleanly and the server stays up | pass | 9 |
| E37 | a secret with 1000 keys round-trips | pass | 59 |
| E38 | 20 concurrent writes to one secret all land (no lost versions) | pass | 766 |
| E39 | of 10 concurrent create-if-absent (cas=0) writes exactly one wins | pass | 50 |
| E40 | secrets are encrypted at rest (no plaintext in the Raft files) | pass | 73 |
| E41 | secret values never reach the audit or server logs | pass | 61 |
| E42 | native gRPC writes and gRPC-Web reads see the same data | pass | 55 |
| F01 | admins can manage servers | pass | 2 |
| F02 | a server needs at least one account | pass | 2 |
| F03 | a host with spaces is refused | pass | 1 |
| F04 | a server needs a name | pass | 5 |
| F05 | an account without a password or key is refused | pass | 1 |
| F06 | an invalid account name is refused | pass | 1 |
| F07 | a port above 65535 is refused | pass | 1 |
| F08 | "Test & save" logs in, pins the host key and saves | pass | 1494 |
| F09 | "Test & save" with a wrong password saves nothing | pass | 77 |
| F10 | "Test & save" against a closed port saves nothing | pass | 6 |
| F11 | two servers cannot share a name | pass | 87 |
| F12 | the server id is a slug of its name | pass | 44 |
| F13 | stored credentials are never returned | pass | 53 |
| F14 | testing a saved server pins its host key on first contact | pass | 129 |
| F15 | a changed host key is detected (possible MITM) and the reset re-pins it | pass | 504 |
| F16 | updating a server without new secrets keeps the stored credentials | pass | 55 |
| F17 | removing an account from a server removes access as it | pass | 46 |
| F18 | moving a server to another host clears its pinned key | pass | 35 |
| F19 | key-based accounts log in with a private key | pass | 23 |
| F20 | a passphrase-protected key works with its passphrase | pass | 1574 |
| F21 | without a grant, a read-only user sees no servers | pass | 491 |
| F22 | a user grant shows the server with the allowed accounts | pass | 494 |
| F23 | a grant can be limited to some accounts | pass | 506 |
| F24 | a role grant applies to everyone with that role | pass | 500 |
| F25 | a grant for an unknown user is refused | pass | 9 |
| F26 | a grant for an account the server lacks is refused | pass | 9 |
| F27 | an expiring grant reports its expiry; hours=0 means permanent | pass | 29 |
| F28 | deleting a grant revokes access | pass | 502 |
| F29 | read-only users cannot create servers or grants | pass | 2 |
| F30 | deleting a server deletes its grants (a new server with the same name starts clean) | pass | 38 |
| F31 | the terminal refuses a connection without a token (401) | pass | 10 |
| F32 | the terminal refuses a user without a grant (403) | pass | 11 |
| F33 | a terminal session runs commands and follows resizes | pass | 831 |
| F34 | a 200 KiB burst of output arrives intact | pass | 378 |
| F35 | a finished session is listed and its recording replays the output | pass | 528 |
| F36 | an administrator can kill a live session | pass | 2185 |
| F37 | users see only their own sessions and can't replay others' | pass | 1039 |
| F38 | commands typed in a terminal are logged per server, newest first, with risk levels | pass | 829 |
| F39 | the command log can be searched and narrowed to risky commands | pass | 818 |
| F40 | people see their own commands only, never someone else's | pass | 1487 |
| F41 | a new account with a generated password is created on the server and works | pass | 361 |
| F42 | if the admin login fails, nothing is created or saved | pass | 131 |
| F43 | an existing account's password can be reset on the server | pass | 116 |
| F44 | creating an account on the server needs a password and an account to sign in with | pass | 2 |
| F45 | a saved account can change its own password on the server (signing in with the current one) | pass | 255 |
| F46 | a new account gets passwordless sudo and the groups that exist | pass | 33 |
| F47 | an existing account's sudo and groups change without touching its password | pass | 58 |
| F48 | unsafe group names and unknown sudo levels are refused | pass | 2 |
| G01 | a single node is its own leader | pass | 1 |
| G02 | a snapshot streams the whole (encrypted) key space as JSON | pass | 42 |
| G03 | snapshots are admin-only | pass | 1 |
| G04 | removing an unknown peer is NOT_FOUND; removing the leader itself is refused | pass | 1 |
| G05 | a join request with a bad node descriptor is refused | pass | 1 |
| G06 | a join answer without a challenge is refused | pass | 1 |
| G07 | three nodes form a cluster with retry_join + `operator unseal --all` | pass | 753 |
| G08 | followers report standby (429) for health; ?standbyok=true makes it 200 | pass | 12 |
| G09 | writes on one follower are immediately readable on another | pass | 676 |
| G10 | the cluster survives losing its leader; the old leader rejoins and catches up | pass | 3889 |
| H01 | every audited call has a request and a response entry with the same id | pass | 8 |
| H02 | tokens appear only as HMACs matching AuditHash; never in the clear | pass | 10 |
| H03 | failed calls are audited with their gRPC status and message | pass | 4 |
| H04 | probe/polling calls are not audited | pass | 5 |
| H05 | a sign-in is audited with who signed in, but never the password | pass | 481 |
| H06 | `operator audit-verify` confirms an intact hash chain | pass | 58 |
| H07 | `operator audit-verify` detects an edited entry | pass | 224 |
| H08 | fail-closed: with no working audit sink, audited calls are refused (UNAVAILABLE) | pass | 165 |
| H09 | `operator status`, `instances` and `health` talk to a node | pass | 46 |
| H10 | `operator` with an unknown command exits 2 | pass | 8 |
| H11 | `operator seal` needs $TIMIKA_TOKEN | pass | 231 |
| H12 | `operator init` + `unseal` (key on stdin) bring up a vault; a refused key exits 1 | pass | 244 |
| H13 | TLS: the CLI and browsers need the CA, or --tls-skip-verify for the CLI | pass | 578 |
| I01 | the `ssh` role is offered and can be assigned | pass | 240 |
| I02 | an ssh-only user cannot read vault data | pass | 463 |
| I03 | an ssh-only user sees and opens only granted servers | pass | 811 |
| I04 | an ssh-only user lists their own sessions and cannot manage servers | pass | 543 |
| I05 | all grants across servers are listed for administrators | pass | 479 |
| I06 | the audit trail is for administrators only | pass | 2 |
| I07 | a secret write shows who, what, the path and the outcome | pass | 26 |
| I08 | a failed sign-in names the targeted account | pass | 240 |
| I09 | successful routine reads are hidden unless asked for | pass | 37 |
| I10 | filter by user returns only that person | pass | 20 |
| I11 | filter by category and outcome | pass | 18 |
| I12 | paging with beforeSeq continues without overlap | pass | 56 |
| I13 | the trail never contains secret values or passwords | pass | 259 |
| I14 | creating a user records the role change but not the password | pass | 248 |
| I15 | granting access records who got which server | pass | 506 |
| I16 | terminal sessions appear in the trail with account@server | pass | 378 |
| I17 | very long targets are truncated | pass | 41 |
| I18 | verify confirms the chain over the API | pass | 63 |
| I19 | without a local audit file the trail says so | pass | 202 |
| I20 | audit events carry their instance and stay newest first | pass | 6 |
| I21 | a server's activity covers its setup, access, tests, terminals and sessions | pass | 875 |
| I22 | a server's activity excludes other servers, even ones with a longer similar id | pass | 63 |
| J01 | browsing starts in the home folder and lists files and folders, folders first | pass | 36 |
| J02 | an upload streams onto the server | pass | 32 |
| J03 | a download link streams the file with its name | pass | 33 |
| J04 | a tampered or made-up download link is refused | pass | 31 |
| J05 | a 12 MiB file goes up and comes back byte for byte | pass | 473 |
| J06 | new folder, rename and recursive delete | pass | 44 |
| J07 | renaming onto an existing name is refused; deleting / is refused | pass | 33 |
| J08 | missing files are NOT_FOUND | pass | 33 |
| J09 | folders cannot be downloaded as a file | pass | 29 |
| J10 | without access to the account there are no files | pass | 491 |
| J11 | a grant to one account gives files as that account only | pass | 520 |
| J12 | file operations appear in the server's activity | pass | 54 |
| J13 | uploads over the size limit are refused and leave nothing behind | pass | 459 |
| J14 | browsing reuses one connection (fast after the first call) | pass | 60 |
| J15 | compress a folder and a file into a .tar.gz on the server | pass | 57 |
| J16 | compress as .zip, .tar.xz and .tar | pass | 89 |
| J17 | compressing onto an existing archive name is refused | pass | 48 |
| J18 | extracting makes a new folder named after the archive, then "name (2)" | pass | 76 |
| J19 | zip archives extract too | pass | 56 |
| J20 | a folder downloads as one streamed .tar.gz (nothing left on the server) | pass | 62 |
| J21 | a selection downloads as one .zip | pass | 57 |
| J22 | odd file names (-rf, quotes, spaces) are archived as names, never as options or code | pass | 51 |
| J23 | archives refuse names outside the folder and non-archives | pass | 23 |
| J24 | compressing and archive downloads are in the audit trail | pass | 81 |
| K01 | setup gives a secret, an otpauth URI and a QR code; 2FA stays off until confirmed | pass | 516 |
| K02 | confirming needs a right code, then gives 10 one-time recovery codes | pass | 498 |
| K03 | sign-in with 2FA: password, then the code, then the session | pass | 749 |
| K04 | a code works once (no replay) | pass | 986 |
| K05 | five wrong codes end the attempt; the password is needed again | pass | 780 |
| K06 | wrong codes count toward account lockout | pass | 992 |
| K07 | a recovery code signs in once | pass | 983 |
| K08 | a made-up 2FA token is refused | pass | 1 |
| K09 | turning 2FA off needs a valid code; then sign-in is password-only | pass | 733 |
| K10 | new recovery codes replace the old ones | pass | 993 |
| K11 | MFA_REQUIRED=all: sign-in without 2FA must set it up first, then gets a full session | pass | 732 |
| K12 | MFA_REQUIRED=admins: admins must, others may; required 2FA cannot be turned off | pass | 1436 |
| K13 | an admin can reset a lost 2FA; People shows who has it | pass | 745 |
| K14 | the root token is unaffected and has no 2FA of its own | pass | 4 |
| K15 | the 2FA secret and codes never reach the audit log | pass | 995 |
| L01 | connecting a repository clones it, finds the default branch and its Terraform and Ansible projects (not modules) | pass | 2143 |
| L02 | a repository that can't be read, or a bad URL, is refused and nothing is saved | pass | 169 |
| L03 | local file:// repositories are refused unless explicitly allowed | pass | 1 |
| L04 | readers see repositories and runs; only admins connect, run and cancel; the ssh role sees nothing | pass | 303 |
| L05 | Plan runs on the runner server and reports +1 ~0 -0; secret variables are masked and the run folder is removed | pass | 746 |
| L06 | Apply applies exactly the reviewed plan, once; state stays in the vault so the next plan shows no changes | pass | 1299 |
| L07 | Apply needs a plan: none, a failed one or another project’s is refused | pass | 442 |
| L08 | Pull picks up new commits and projects; runs use the head at the time | pass | 785 |
| L09 | push webhooks: bad signatures refused, ping answered, other branches ignored, pushes plan only the projects they touch | pass | 691 |
| L10 | Ansible: check runs the playbook in check mode, run applies it | pass | 2028 |
| L11 | a running run can be cancelled from any page; a second run of the same project waits its turn | pass | 19551 |
| L12 | an unreachable runner fails the run with a clear error | pass | 499 |
| L13 | watching a run streams all of its output and ends with the final status | pass | 701 |
| L14 | secret variables are never returned; saving without a value keeps them; bad names are refused | pass | 643 |
| L15 | deploy keys: a generated key is kept with the repository; unknown keys are refused | pass | 284 |
| L16 | the runner check lists the tools installed on the runner server | pass | 285 |
| L17 | the audit log records repository changes, runs and webhooks — never secret values | pass | 609 |
| L18 | removing a repository deletes its runs, output, saved plans and state | pass | 668 |
| L19 | run options: Terraform workspaces keep separate state; a destroy plan removes what apply created | pass | 2326 |
| L20 | run options: Ansible against timika’s servers by tag — host keys checked against the pins, passwords masked | pass | 1128 |
| L21 | approvals: Apply / Run wait for another admin; the requester can’t approve; rejecting frees the plan | pass | 3178 |
| L22 | schedules: validated, fire on time exactly once with trigger "schedule", report drift, pause and run now | pass | 48887 |
| L23 | notifications: targets validated and never returned; test message; failed runs and approvals are announced | pass | 539 |
| L24 | CI trigger: bearer token, project by name, wait for the result (200 / 409), rotate | pass | 4718 |
| L25 | the file viewer lists folders and reads files at the pulled commit; paths, binaries and big files are handled; admins only | pass | 1164 |
| L26 | a tool missing on the runner fails the run and names the tool | pass | 427 |
| M01 | monitoring a server reads its numbers over SSH; rates (CPU, network) come from two readings | pass | 2204 |
| M02 | history is kept and returned oldest first for a range; disks, containers and failed services are shown | pass | 4663 |
| M03 | a server that is not Linux, or stops answering, shows as down with the reason | pass | 5293 |
| M04 | alerts: a server’s own rule fires once when the average crosses the threshold, and is announced | pass | 45164 |
| M05 | people see only the servers they may use; only admins choose what is monitored and how | pass | 997 |
| M06 | stopping monitoring deletes the history; deleting the server stops monitoring it | pass | 3537 |
| M07 | restarting timika costs one reading, not the rates: the first reading afterwards continues from the stored counters | pass | 2367 |
| M08 | containers: image, status, ports, health and network rates; history per container; logs and start / stop / restart for admins only | pass | 3334 |
| M09 | container usage: disk I/O rate, processes and size on disk per container; its own history for a range; only for people with access | pass | 2302 |
| N01 | a container’s details: image, command, environment, mounts, networks, published ports, compose | pass | 1419 |
| N02 | files in a container: browse folders, hidden files, sizes and modes; missing folders and shell-less images are explained | pass | 850 |
| N03 | transfer: upload a file and a folder into a container, download a file byte for byte and a folder as a tar | pass | 361 |
| N04 | new folder and delete inside a container; / can’t be deleted | pass | 92 |
| N05 | images and volumes: size, what uses them, remove and clean up — the runtime’s refusals are shown | pass | 217 |
| N06 | Container management is for administrators: readers and people with access to the server are refused everywhere, also for a shell | pass | 509 |
| N07 | Podman: a server that runs podman is driven with podman — files, logs, images, start / stop — and says so | pass | 1452 |
