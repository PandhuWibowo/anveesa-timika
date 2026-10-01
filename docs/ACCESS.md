# Access — servers, terminals, files, people

timika is also a **bastion** (a jump server): people reach servers through it,
never with SSH keys or passwords on their own laptops, and everything they do is
recorded. This guide covers the **Access** section of the UI and how it works.

| Page | What it's for |
|---|---|
| **Servers** | Every server you may use. One click opens a terminal. Admins add and edit servers. |
| **Terminal** | Your open terminal tabs. They keep running while you use other pages. |
| **Sessions** | Every recorded terminal session: replay it, download it, end it. |
| **People** | Who can sign in, their role, 2FA, and which servers they reach (admins). |
| **Audit trail** | Who did what, when, from where — tamper-evident (admins). |
| **My account** | Your password and two-factor authentication (click your name, top right). |

Each server also has its own page (click its name): **Commands**, **Files**,
**Sessions**, **Activity** and **Who has access**.

---

## Servers

**Add server** (admins) → name, host, port, and at least one account:

- **Password** or **private key** (with passphrase). Credentials are stored
  encrypted in the vault and never shown again — leave the field empty when
  editing to keep them.
- **Test & save** signs in once, checks the credentials and **pins the host
  key**. Every later connection checks the server presents the same key; a
  different key is refused (*HOST KEY CHANGED* — possible man-in-the-middle).
  If a server was legitimately reinstalled: *Edit → Host key → Reset*.

**Connect** opens a terminal tab (*Connect ▾* when you have several accounts).
**⌘K** → type a server name → *Connect to account@server* works from anywhere.

### Accounts on the server itself

The account cards in *Edit* can also **manage the account on the VM**, signing
in as `root` (or a sudo user, or the account itself):

| You want to… | Do this |
|---|---|
| Create a new Linux user | *+ Add account* → username → **Generate** password → *Create on the server* (on by default) → *Test & save* |
| Change a saved account's password | *Manage on server* → Password: **Generate new** / **Type new** |
| Give sudo | *Manage on server* → Sudo: **Sudo** (asks for the password) or **Sudo, no password** |
| Add to groups | *Manage on server* → Groups: type, or click *+ docker*, *+ adm*… |

timika runs `useradd`, `chpasswd`, `usermod -aG` and writes a sudo rule to
`/etc/sudoers.d/timika-<user>` that must pass `visudo -c` before it's installed
(a bad rule can never break sudo). The password never appears on a command line.
New passwords are tested by signing in with them before anything is saved; the
confirmation screen shows them once, to copy.

---

## Terminal

- Tabs per session, status dot (green live · amber connecting · red failed).
  **Reconnect** if a connection drops.
- **⤢ Full screen** (or **Ctrl+Shift+F**) shows only the terminal. In Chrome /
  Edge / Brave / Arc, **Esc** goes to the terminal (vim works) — *hold Esc* to
  leave full screen.
- **Ctrl+Shift+[ / ]** switch tabs. **📂** opens the Files of the same server,
  as the same account.
- Every session is **recorded** (what appeared on screen, asciicast format).

---

## Commands

*Server → Commands* lists the commands people ran, newest first: when, who,
which account, the command. **Search**, or tick **risky only**:

- **high** (red): `rm -rf`, `shutdown` / `reboot`, `mkfs`, `dd … of=/dev/…`,
  `curl … | sh`, `chmod -R 777`, `iptables -F`, `userdel`, `crontab -r`…
- **medium** (amber): `sudo`, `systemctl restart|stop`, `kill`, package
  removal, `docker rm`, `kubectl delete`, `git push --force`…

**▶** replays the session from a moment before that command. The player also
lists the session's commands — click one to jump there.

How commands are captured: when you press Enter, timika takes the line **as the
screen shows it** once the server echoes the line break — so tab completion and
↑ history come out right, and fast typing or a pasted command is fine.

- **Never captured:** anything the server didn't echo — the password at a
  `sudo` / `passwd` prompt. The log contains nothing that isn't already in the
  recording.
- Full-screen programs (vim, less, top) are skipped.
- The list refreshes every 5 seconds (and when you come back to the tab).

---

## Files (SFTP)

*Server → Files* (or 📂 on a server card / in the terminal bar), as an account
you may use:

- Browse (click a folder; type a path into the path bar), **Upload** (button or
  drag & drop, with progress), **Download** (straight to disk), **New folder**,
  **Rename**, **Delete** (folders with everything inside, after confirming),
  **Show hidden**, multi-select.
- **Compress…** → `.zip` · `.tar.gz` · `.tar.xz` · `.tar` — made **on the server**
  by its own `tar` / `zip`; nothing passes through your laptop.
- **Extract** (on `.zip` / `.tar*` files) → into a new folder named after the
  archive (`site/`, then `site (2)/`) — never over existing files.
- **Download as .zip / .tar.gz** — a folder or a selection, streamed straight
  from the server; nothing is left on it.

Downloads use a signed link valid for one minute, for one file, for you.
Uploads are capped by `FILES_MAX_UPLOAD_MB` (default 4096). File names like
`-rf` or `a'; rm -rf ~` are always just names.

---

## Sessions & recordings

*Sessions* (and *Server → Sessions*): status (live · closed · killed · failed ·
lost), who, server, account, client IP, duration, command count, output size.

- **▶ Replay** — original timing (long pauses shortened), 1× – 8×, seek, space to
  pause. **⬇** downloads the asciinema `.cast` file.
- **■ End session** (admins) — the person's terminal closes within ~2 s.
- Admins see everyone's sessions; others see their own.
- Recordings and command logs are deleted after `SESSION_RETENTION_DAYS` (90).
  A recording stops at `RECORDING_MAX_MB` (100) per session.

---

## People, roles & access

| Role | Can |
|---|---|
| **SSH access** (`ssh`) | Only the servers granted to them — no vault data |
| **Vault reader** (`read-only`) | Vault status and reads, plus granted servers |
| **Administrator** (`admin`) | Everything, every server |

**Add user** is one form: username, role, password (**Generate** — shown once
with *Copy sign-in details* — or type one), and which servers they get (all
accounts or some, permanent or for 1 h · 8 h · 1 day · 7 / 30 days). By default
they choose their own password at first sign-in.

Click a person for: role, disable / enable, unlock, **Reset 2FA** (lost phone),
delete; set a new password; their servers (grant / revoke); their recent
activity. Server access can also be granted per server (*Edit → Access*) — to a
user or to everyone with a role.

## Two-factor authentication

Authenticator-app codes (TOTP) + 10 one-time recovery codes. Turn it on in
**My account**; require it with `MFA_REQUIRED=admins|all`. Details:
[LOGIN.md](LOGIN.md#two-factor-authentication-mfa).

## Audit trail

*Audit trail* (admins) — every sign-in, server change, access grant, terminal,
file transfer, archive and command-log view, in plain language: who, what, the
target (a server, user or file path — never a secret or password), the result
and the IP. Filter by category (Sign-in · Servers · Sessions · Files · Users ·
System), result or person; **Live** refreshes it; **Verify integrity** checks
the hash chain; **Export CSV**. Each server's **Activity** tab is the same
trail, for that server. Details: [LOGGING.md](LOGGING.md).

---

## Settings

| Variable | Default | |
|---|---|---|
| `SESSION_RETENTION_DAYS` | 90 | Delete recordings / command logs older than this |
| `RECORDING_MAX_MB` | 100 | Per-session recording limit |
| `FILES_MAX_UPLOAD_MB` | 4096 | Upload size limit |
| `MFA_REQUIRED` | off | `admins` / `all`: required two-factor |
| `MFA_ISSUER` | Timika | Name in authenticator apps |

## Limits (today)

- Commands are captured in web terminals only; someone who SSHes to a server
  directly (bypassing the bastion) isn't — firewall servers to accept SSH only
  from timika for full coverage. Several commands pasted at once may log only
  the first.
- No in-browser file editor or folder upload yet.
- Archive operations need `tar` (always) / `zip`, `unzip` (often not installed —
  you're told) on the server.
