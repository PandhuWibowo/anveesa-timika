# Nginx — sites, config with a safety net, certificates, logs

*Web → Nginx* manages the nginx on your monitored servers, whether it is
installed **on the server** or runs **in a container** (Docker or Podman).
There is nothing to set up: monitoring finds it, and it appears by itself.

It deliberately does **not** repeat what timika already has. CPU and memory are
under *Monitoring*, a shell is a *Terminal* (or the container's Shell), container
logs and start / stop are under *Containers* — the Nginx page links to them.

| | |
|---|---|
| **Sites** | Every `server { }` block: names, ports, TLS, what it serves (proxy → where, static → folder, redirect → to), its certificate's days left, the file and line. Sites that are switched off are listed too. |
| **Edit** | Click a site or a file. **Test & apply** writes the file, runs `nginx -t` and reloads. If nginx rejects it, the old file is put back on the server and nothing changes; the error names the line. |
| **History** | Every applied version of a file is kept (encrypted, in the vault) with who and when — plus the content found on the server when it was changed outside timika. Open a version and apply it to go back. |
| **New site** | Reverse proxy, static files or redirect from a domain and one more field; you see and can edit the result before it is applied. |
| **On / off** | A switch per file: the `sites-enabled` link on Debian-style layouts, a `.disabled` suffix elsewhere (`conf.d`). Tested and reloaded like an edit. |
| **Certificates** | The certificate files nginx uses, who they are for, who issued them, days left, which sites use them. |
| **Logs** | The log files named in the configuration, last 100–2000 lines, live refresh. In a container whose logs go to stdout, the container's output opens instead. |
| **Test & reload** | `nginx -t`, then `nginx -s reload`. |

## How it works

- **Finding nginx.** Each monitoring reading notes nginx on the server
  (`nginx -v`, and whether it runs) and every container whose image is nginx or
  OpenResty. The list of nginx instances comes from that reading: no SSH, instant.
- **Opening one** is a single script over SSH: `nginx -V` (where the config is),
  `nginx -T` (the whole running configuration), the files in the config folder,
  and the sites that are switched off. timika parses the `server` blocks itself.
  The result is kept for half a minute and refreshed after every change.
- **Changing something** is one script that does the change, runs `nginx -t`,
  and either reloads or undoes its own change — on the server, so a dropped
  connection can't leave a half-applied state behind a failed test.
- **As whom.** The server's monitoring account. For nginx on the server it must
  be root or have passwordless `sudo`; for a container it must be allowed to run
  `docker` / `podman` (commands run through `exec`).
- **In a container,** files are written in place, so a bind-mounted config file
  keeps working. Files that are not on a volume or bind mount last only as long
  as the container — the editor says so.

## Safety

- Admin only, like everything that changes a server.
- No free-form commands: fixed scripts, every path and name one quoted word.
- Only files inside nginx's own configuration folder (checked on the server
  against `--conf-path`); logs only from `/var/log`, the config folder or
  nginx's `logs` folder.
- Certificates and keys (`.key`, `.pem`, `.crt` …, or anything containing a
  private key) are never read into the browser, written, or kept in the history.
- A save is refused when the file changed on the server since you opened it.
- At most 256 KiB per file; 20 versions (1 MiB) of history per file. History is
  deleted with the server.
- Audited: who read, changed, switched, deleted or reloaded what — never the
  content.

## API

`NginxService` (admin): ListInstances · GetInstance · ReadFile · SaveFile ·
SetEnabled · DeleteFile · Reload · ListVersions · GetVersion · ReadLog.

## Limits (today)

- No Let's Encrypt issuing or renewal: timika shows expiry, it does not request
  certificates.
- No copying a site to other servers, and no side-by-side diff between versions.
- Plain text editor (line numbers, jump to the failing line), no syntax
  highlighting.
- Containers are recognised by image name (`nginx`, `openresty`); nginx inside
  an image with another name is not listed.
- The parser reads `server` blocks for the overview; Lua blocks and unusual
  constructs may show less detail, but never block editing.
