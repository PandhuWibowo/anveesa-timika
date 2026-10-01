# timika e2e report

296/296 passed · 2026-10-01T08:51:06.896Z

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
| L · Infrastructure automation | 24/24 |

| ID | Scenario | Result | ms |
|---|---|---|---|
| A01 | health is 501 on an uninitialized node | pass | 173 |
| A02 | health ?uninitok=true turns 501 into 200 | pass | 1 |
| A03 | health is 503 while sealed | pass | 194 |
| A04 | health ?sealedok=true turns 503 into 200 | pass | 1 |
| A05 | health is 200 on the unsealed leader, with engine and node | pass | 1604 |
| A06 | health reports a semver version | pass | 1 |
| A07 | grpc.health.v1 says NOT_SERVING while sealed | pass | 15 |
| A08 | grpc.health.v1 says SERVING once unsealed | pass | 485 |
| A09 | grpc.health.v1 answers over gRPC-Web too (browser-based probes) | pass | 3 |
| A10 | native gRPC (HTTP/2) and gRPC-Web return the same seal status | pass | 10 |
| A11 | an unknown method returns UNIMPLEMENTED | pass | 2 |
| A12 | an unknown service returns UNIMPLEMENTED, not the SPA | pass | 1 |
| A13 | a malformed protobuf body is rejected and the server stays up | pass | 2 |
| A14 | CORS preflight for gRPC-Web is allowed | pass | 1 |
| A15 | CORS exposes the grpc-status header to browsers | pass | 1 |
| A16 | the SPA is served for / and for client-side routes | pass | 3 |
| A17 | an unknown /v1 path is a 404, not the SPA | pass | 1 |
| A18 | 200 concurrent gRPC-Web calls all succeed | pass | 107 |
| A19 | responses carry an x-timika-request-id for correlation with the audit log | pass | 1 |
| A20 | a call to a sealed node fails with FAILED_PRECONDITION "vault is sealed" | pass | 1 |
| B01 | status of a new node: uninitialized, sealed, raft, node name | pass | 161 |
| B02 | init rejects threshold > shares | pass | 162 |
| B03 | init rejects zero shares | pass | 162 |
| B04 | init rejects more than 16 shares | pass | 160 |
| B05 | init rejects share counts that overflow (300) | pass | 164 |
| B06 | init returns N distinct keys and a tmk. root token, and leaves the vault sealed | pass | 199 |
| B07 | a 1-of-1 vault unseals with its single key | pass | 204 |
| B08 | init twice is refused | pass | 192 |
| B09 | unseal rejects a key that is not base64 | pass | 199 |
| B10 | unseal rejects base64 that is not a share | pass | 195 |
| B11 | one key moves progress to 1 of the threshold | pass | 196 |
| B12 | submitting the same key twice does not count twice | pass | 202 |
| B13 | reset clears unseal progress | pass | 194 |
| B14 | another vault's keys are rejected and progress resets | pass | 386 |
| B15 | unseal on an uninitialized node is FAILED_PRECONDITION | pass | 162 |
| B16 | unseal on an unsealed vault is a harmless no-op | pass | 204 |
| B17 | any threshold-sized subset of keys unseals (keys 2 and 3) | pass | 201 |
| B18 | seal without a token is UNAUTHENTICATED | pass | 203 |
| B19 | seal with a read-only token is PERMISSION_DENIED | pass | 685 |
| B20 | seal by root seals; data calls then fail with "vault is sealed" | pass | 205 |
| B21 | a sealed vault reports sealed, not "bad token", for token calls | pass | 199 |
| B22 | seal → unseal keeps every secret | pass | 228 |
| B23 | a restarted process comes back sealed; its data survives | pass | 392 |
| B24 | rotate adds a key term; old and new secrets stay readable | pass | 247 |
| B25 | a rotated keyring survives seal + unseal | pass | 261 |
| C01 | login config is public and names the policy profile and captcha | pass | 1 |
| C02 | the cn-mlps profile is reported with its own rules | pass | 160 |
| C03 | login config never exposes captcha secrets | pass | 161 |
| C04 | a captcha challenge is signed and expires | pass | 1 |
| C05 | no captcha challenges while sealed (the key is derived from the root key) | pass | 194 |
| C06 | login without a captcha is refused | pass | 1 |
| C07 | a captcha for a different provider is refused | pass | 1 |
| C08 | a wrong proof-of-work answer is refused | pass | 3 |
| C09 | a solved captcha cannot be replayed | pass | 240 |
| C10 | a self-made challenge with a forged signature is refused | pass | 1 |
| C11 | the system-use notice must be acknowledged (us-nist) | pass | 2 |
| C12 | a good sign-in returns a session token with expiry | pass | 235 |
| C13 | a wrong password is UNAUTHENTICATED with a generic message | pass | 236 |
| C14 | an unknown user gets exactly the same answer (no user enumeration) | pass | 457 |
| C15 | an impossible username gets the same generic answer | pass | 223 |
| C16 | usernames are case-insensitive at sign-in | pass | 233 |
| C17 | the account locks after the threshold of failures, even for the right password | pass | 1404 |
| C18 | an administrator can unlock a locked account | pass | 1422 |
| C19 | a successful sign-in resets the failure counter | pass | 1923 |
| C20 | a disabled user cannot sign in (generic message) | pass | 699 |
| C21 | disabling a user ends their live sessions immediately | pass | 487 |
| C22 | deleting a user ends their live sessions immediately | pass | 476 |
| C23 | a new user must change the admin-set password at first sign-in | pass | 470 |
| C24 | a change-password-only session cannot read secrets | pass | 476 |
| C25 | a change-password-only session can look itself up | pass | 464 |
| C26 | change password with the wrong current password is refused | pass | 222 |
| C27 | the new password must differ from the current one | pass | 444 |
| C28 | the new password must meet the policy | pass | 1 |
| C29 | changing the password ends the session; the new password grants full access | pass | 1379 |
| C30 | cn-mlps: a recently used password cannot be reused | pass | 2278 |
| C31 | the root token has no password to change | pass | 1 |
| C32 | lookup-self on the root token: root policy, no user, no expiry | pass | 1 |
| C33 | the root token is not renewable; a session is | pass | 1145 |
| C34 | revoke-self ends the session | pass | 294 |
| C35 | tokens work as `authorization: Bearer` too | pass | 1 |
| C36 | a made-up token is UNAUTHENTICATED | pass | 1 |
| C37 | sign-in attempts are rate limited per client address | pass | 899 |
| C38 | the second sign-in reports when and from where the previous one was | pass | 715 |
| D01 | admins list users and the assignable roles | pass | 3 |
| D02 | read-only users cannot list users | pass | 1 |
| D03 | an invalid username is refused | pass | 1 |
| D04 | a new user needs a password | pass | 1 |
| D05 | an unknown role is refused | pass | 1 |
| D06 | a weak password is refused when creating a user | pass | 1 |
| D07 | a password containing the username is refused | pass | 1 |
| D08 | get-user returns the profile and never a password hash | pass | 1 |
| D09 | get-user for an unknown name is NOT_FOUND | pass | 1 |
| D10 | changing only the roles keeps the password | pass | 473 |
| D11 | an administrator cannot delete their own account | pass | 1 |
| D12 | deleting an unknown user is NOT_FOUND | pass | 1 |
| D13 | usernames are stored lower-case | pass | 232 |
| D14 | read-only users cannot create users | pass | 1 |
| D15 | unlocking an unknown user is NOT_FOUND | pass | 1 |
| E01 | write then read a secret | pass | 11 |
| E02 | JSON types survive the round trip (numbers, bools, null, nested, arrays) | pass | 14 |
| E03 | unicode and emoji values survive | pass | 13 |
| E04 | versions count up with every write | pass | 36 |
| E05 | an older version can be read | pass | 25 |
| E06 | reading a secret that does not exist is NOT_FOUND | pass | 1 |
| E07 | reading a version that does not exist is NOT_FOUND | pass | 14 |
| E08 | cas=0 creates a new secret | pass | 14 |
| E09 | cas=0 refuses to overwrite an existing secret (ABORTED) | pass | 14 |
| E10 | cas=current succeeds | pass | 23 |
| E11 | a stale cas is ABORTED | pass | 31 |
| E12 | `..` in a path is refused | pass | 1 |
| E13 | an empty path is refused | pass | 2 |
| E14 | spaces in a path are refused | pass | 1 |
| E15 | leading and trailing slashes are ignored | pass | 13 |
| E16 | an empty segment (a//b) is refused | pass | 1 |
| E17 | non-ASCII path segments are refused | pass | 1 |
| E18 | a `.` segment is refused | pass | 1 |
| E19 | dots, dashes and underscores are fine in names | pass | 14 |
| E20 | listing a folder shows sub-folders with a trailing slash and secrets | pass | 28 |
| E21 | a listing shows only immediate children, once each | pass | 71 |
| E22 | the root listing includes top-level folders | pass | 15 |
| E23 | listing a folder that does not exist is NOT_FOUND | pass | 2 |
| E24 | soft delete with no versions deletes the latest | pass | 47 |
| E25 | soft delete of chosen versions leaves the others | pass | 58 |
| E26 | writing after a soft delete makes a new readable version | pass | 46 |
| E27 | destroy removes every version and the metadata | pass | 55 |
| E28 | destroying a secret that does not exist is NOT_FOUND | pass | 2 |
| E29 | old versions are pruned beyond KV_MAX_VERSIONS | pass | 294 |
| E30 | metadata carries versions and timestamps in order | pass | 34 |
| E31 | read-only users can read and list | pass | 24 |
| E32 | read-only users cannot write | pass | 2 |
| E33 | read-only users cannot delete or destroy | pass | 17 |
| E34 | every KV call needs a token | pass | 3 |
| E35 | a 1 MiB value round-trips | pass | 529 |
| E36 | an oversized (8 MiB) write is refused cleanly and the server stays up | pass | 20 |
| E37 | a secret with 1000 keys round-trips | pass | 27 |
| E38 | 20 concurrent writes to one secret all land (no lost versions) | pass | 232 |
| E39 | of 10 concurrent create-if-absent (cas=0) writes exactly one wins | pass | 17 |
| E40 | secrets are encrypted at rest (no plaintext in the Raft files) | pass | 30 |
| E41 | secret values never reach the audit or server logs | pass | 19 |
| E42 | native gRPC writes and gRPC-Web reads see the same data | pass | 22 |
| F01 | admins can manage servers | pass | 2 |
| F02 | a server needs at least one account | pass | 2 |
| F03 | a host with spaces is refused | pass | 1 |
| F04 | a server needs a name | pass | 1 |
| F05 | an account without a password or key is refused | pass | 1 |
| F06 | an invalid account name is refused | pass | 1 |
| F07 | a port above 65535 is refused | pass | 1 |
| F08 | "Test & save" logs in, pins the host key and saves | pass | 217 |
| F09 | "Test & save" with a wrong password saves nothing | pass | 69 |
| F10 | "Test & save" against a closed port saves nothing | pass | 5 |
| F11 | two servers cannot share a name | pass | 27 |
| F12 | the server id is a slug of its name | pass | 15 |
| F13 | stored credentials are never returned | pass | 15 |
| F14 | testing a saved server pins its host key on first contact | pass | 49 |
| F15 | a changed host key is detected (possible MITM) and the reset re-pins it | pass | 482 |
| F16 | updating a server without new secrets keeps the stored credentials | pass | 60 |
| F17 | removing an account from a server removes access as it | pass | 47 |
| F18 | moving a server to another host clears its pinned key | pass | 36 |
| F19 | key-based accounts log in with a private key | pass | 25 |
| F20 | a passphrase-protected key works with its passphrase | pass | 1337 |
| F21 | without a grant, a read-only user sees no servers | pass | 485 |
| F22 | a user grant shows the server with the allowed accounts | pass | 492 |
| F23 | a grant can be limited to some accounts | pass | 520 |
| F24 | a role grant applies to everyone with that role | pass | 508 |
| F25 | a grant for an unknown user is refused | pass | 13 |
| F26 | a grant for an account the server lacks is refused | pass | 10 |
| F27 | an expiring grant reports its expiry; hours=0 means permanent | pass | 38 |
| F28 | deleting a grant revokes access | pass | 527 |
| F29 | read-only users cannot create servers or grants | pass | 2 |
| F30 | deleting a server deletes its grants (a new server with the same name starts clean) | pass | 58 |
| F31 | the terminal refuses a connection without a token (401) | pass | 12 |
| F32 | the terminal refuses a user without a grant (403) | pass | 14 |
| F33 | a terminal session runs commands and follows resizes | pass | 835 |
| F34 | a 200 KiB burst of output arrives intact | pass | 396 |
| F35 | a finished session is listed and its recording replays the output | pass | 519 |
| F36 | an administrator can kill a live session | pass | 2300 |
| F37 | users see only their own sessions and can't replay others' | pass | 1062 |
| F38 | commands typed in a terminal are logged per server, newest first, with risk levels | pass | 812 |
| F39 | the command log can be searched and narrowed to risky commands | pass | 813 |
| F40 | people see their own commands only, never someone else's | pass | 1479 |
| F41 | a new account with a generated password is created on the server and works | pass | 368 |
| F42 | if the admin login fails, nothing is created or saved | pass | 138 |
| F43 | an existing account's password can be reset on the server | pass | 139 |
| F44 | creating an account on the server needs a password and an account to sign in with | pass | 2 |
| F45 | a saved account can change its own password on the server (signing in with the current one) | pass | 279 |
| F46 | a new account gets passwordless sudo and the groups that exist | pass | 36 |
| F47 | an existing account's sudo and groups change without touching its password | pass | 65 |
| F48 | unsafe group names and unknown sudo levels are refused | pass | 2 |
| G01 | a single node is its own leader | pass | 1 |
| G02 | a snapshot streams the whole (encrypted) key space as JSON | pass | 45 |
| G03 | snapshots are admin-only | pass | 2 |
| G04 | removing an unknown peer is NOT_FOUND; removing the leader itself is refused | pass | 2 |
| G05 | a join request with a bad node descriptor is refused | pass | 1 |
| G06 | a join answer without a challenge is refused | pass | 2 |
| G07 | three nodes form a cluster with retry_join + `operator unseal --all` | pass | 813 |
| G08 | followers report standby (429) for health; ?standbyok=true makes it 200 | pass | 12 |
| G09 | writes on one follower are immediately readable on another | pass | 950 |
| G10 | the cluster survives losing its leader; the old leader rejoins and catches up | pass | 3926 |
| H01 | every audited call has a request and a response entry with the same id | pass | 9 |
| H02 | tokens appear only as HMACs matching AuditHash; never in the clear | pass | 8 |
| H03 | failed calls are audited with their gRPC status and message | pass | 3 |
| H04 | probe/polling calls are not audited | pass | 4 |
| H05 | a sign-in is audited with who signed in, but never the password | pass | 487 |
| H06 | `operator audit-verify` confirms an intact hash chain | pass | 61 |
| H07 | `operator audit-verify` detects an edited entry | pass | 226 |
| H08 | fail-closed: with no working audit sink, audited calls are refused (UNAVAILABLE) | pass | 165 |
| H09 | `operator status`, `instances` and `health` talk to a node | pass | 47 |
| H10 | `operator` with an unknown command exits 2 | pass | 7 |
| H11 | `operator seal` needs $TIMIKA_TOKEN | pass | 230 |
| H12 | `operator init` + `unseal` (key on stdin) bring up a vault; a refused key exits 1 | pass | 240 |
| H13 | TLS: the CLI and browsers need the CA, or --tls-skip-verify for the CLI | pass | 602 |
| I01 | the `ssh` role is offered and can be assigned | pass | 241 |
| I02 | an ssh-only user cannot read vault data | pass | 473 |
| I03 | an ssh-only user sees and opens only granted servers | pass | 1072 |
| I04 | an ssh-only user lists their own sessions and cannot manage servers | pass | 709 |
| I05 | all grants across servers are listed for administrators | pass | 487 |
| I06 | the audit trail is for administrators only | pass | 2 |
| I07 | a secret write shows who, what, the path and the outcome | pass | 27 |
| I08 | a failed sign-in names the targeted account | pass | 239 |
| I09 | successful routine reads are hidden unless asked for | pass | 40 |
| I10 | filter by user returns only that person | pass | 18 |
| I11 | filter by category and outcome | pass | 17 |
| I12 | paging with beforeSeq continues without overlap | pass | 62 |
| I13 | the trail never contains secret values or passwords | pass | 267 |
| I14 | creating a user records the role change but not the password | pass | 251 |
| I15 | granting access records who got which server | pass | 513 |
| I16 | terminal sessions appear in the trail with account@server | pass | 361 |
| I17 | very long targets are truncated | pass | 40 |
| I18 | verify confirms the chain over the API | pass | 62 |
| I19 | without a local audit file the trail says so | pass | 201 |
| I20 | audit events carry their instance and stay newest first | pass | 5 |
| I21 | a server's activity covers its setup, access, tests, terminals and sessions | pass | 864 |
| I22 | a server's activity excludes other servers, even ones with a longer similar id | pass | 69 |
| J01 | browsing starts in the home folder and lists files and folders, folders first | pass | 42 |
| J02 | an upload streams onto the server | pass | 35 |
| J03 | a download link streams the file with its name | pass | 35 |
| J04 | a tampered or made-up download link is refused | pass | 35 |
| J05 | a 12 MiB file goes up and comes back byte for byte | pass | 492 |
| J06 | new folder, rename and recursive delete | pass | 45 |
| J07 | renaming onto an existing name is refused; deleting / is refused | pass | 32 |
| J08 | missing files are NOT_FOUND | pass | 33 |
| J09 | folders cannot be downloaded as a file | pass | 30 |
| J10 | without access to the account there are no files | pass | 532 |
| J11 | a grant to one account gives files as that account only | pass | 665 |
| J12 | file operations appear in the server's activity | pass | 56 |
| J13 | uploads over the size limit are refused and leave nothing behind | pass | 451 |
| J14 | browsing reuses one connection (fast after the first call) | pass | 63 |
| J15 | compress a folder and a file into a .tar.gz on the server | pass | 61 |
| J16 | compress as .zip, .tar.xz and .tar | pass | 102 |
| J17 | compressing onto an existing archive name is refused | pass | 55 |
| J18 | extracting makes a new folder named after the archive, then "name (2)" | pass | 87 |
| J19 | zip archives extract too | pass | 58 |
| J20 | a folder downloads as one streamed .tar.gz (nothing left on the server) | pass | 60 |
| J21 | a selection downloads as one .zip | pass | 54 |
| J22 | odd file names (-rf, quotes, spaces) are archived as names, never as options or code | pass | 54 |
| J23 | archives refuse names outside the folder and non-archives | pass | 26 |
| J24 | compressing and archive downloads are in the audit trail | pass | 85 |
| K01 | setup gives a secret, an otpauth URI and a QR code; 2FA stays off until confirmed | pass | 514 |
| K02 | confirming needs a right code, then gives 10 one-time recovery codes | pass | 506 |
| K03 | sign-in with 2FA: password, then the code, then the session | pass | 762 |
| K04 | a code works once (no replay) | pass | 1002 |
| K05 | five wrong codes end the attempt; the password is needed again | pass | 817 |
| K06 | wrong codes count toward account lockout | pass | 998 |
| K07 | a recovery code signs in once | pass | 994 |
| K08 | a made-up 2FA token is refused | pass | 1 |
| K09 | turning 2FA off needs a valid code; then sign-in is password-only | pass | 749 |
| K10 | new recovery codes replace the old ones | pass | 1004 |
| K11 | MFA_REQUIRED=all: sign-in without 2FA must set it up first, then gets a full session | pass | 742 |
| K12 | MFA_REQUIRED=admins: admins must, others may; required 2FA cannot be turned off | pass | 1455 |
| K13 | an admin can reset a lost 2FA; People shows who has it | pass | 750 |
| K14 | the root token is unaffected and has no 2FA of its own | pass | 5 |
| K15 | the 2FA secret and codes never reach the audit log | pass | 998 |
| L01 | connecting a repository clones it, finds the default branch and its Terraform and Ansible projects (not modules) | pass | 2299 |
| L02 | a repository that can't be read, or a bad URL, is refused and nothing is saved | pass | 195 |
| L03 | local file:// repositories are refused unless explicitly allowed | pass | 1 |
| L04 | readers see repositories and runs; only admins connect, run and cancel; the ssh role sees nothing | pass | 321 |
| L05 | Plan runs on the runner server and reports +1 ~0 -0; secret variables are masked and the run folder is removed | pass | 764 |
| L06 | Apply applies exactly the reviewed plan, once; state stays in the vault so the next plan shows no changes | pass | 1304 |
| L07 | Apply needs a plan: none, a failed one or another project’s is refused | pass | 462 |
| L08 | Pull picks up new commits and projects; runs use the head at the time | pass | 918 |
| L09 | push webhooks: bad signatures refused, ping answered, other branches ignored, pushes plan only the projects they touch | pass | 916 |
| L10 | Ansible: check runs the playbook in check mode, run applies it | pass | 2124 |
| L11 | a running run can be cancelled from any page; a second run of the same project waits its turn | pass | 19554 |
| L12 | an unreachable runner fails the run with a clear error | pass | 541 |
| L13 | watching a run streams all of its output and ends with the final status | pass | 1137 |
| L14 | secret variables are never returned; saving without a value keeps them; bad names are refused | pass | 961 |
| L15 | deploy keys: a generated key is kept with the repository; unknown keys are refused | pass | 334 |
| L16 | the runner check lists the tools installed on the runner server | pass | 356 |
| L17 | the audit log records repository changes, runs and webhooks — never secret values | pass | 635 |
| L18 | removing a repository deletes its runs, output, saved plans and state | pass | 700 |
| L19 | run options: Terraform workspaces keep separate state; a destroy plan removes what apply created | pass | 2377 |
| L20 | run options: Ansible against timika’s servers by tag — host keys checked against the pins, passwords masked | pass | 1166 |
| L21 | approvals: Apply / Run wait for another admin; the requester can’t approve; rejecting frees the plan | pass | 3338 |
| L22 | schedules: validated, fire on time exactly once with trigger "schedule", report drift, pause and run now | pass | 6571 |
| L23 | notifications: targets validated and never returned; test message; failed runs and approvals are announced | pass | 537 |
| L24 | CI trigger: bearer token, project by name, wait for the result (200 / 409), rotate | pass | 4756 |
