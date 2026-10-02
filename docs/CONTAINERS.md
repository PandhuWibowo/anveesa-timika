# Containers — Docker and Podman

The **Containers** menu works with the containers on your monitored servers,
whether a server runs **Docker**, **Podman**, or both. It drives the `docker` /
`podman` command line over the SSH access timika already has, so there is no
socket to expose and no agent to install.

| Page | |
|---|---|
| **Containers** | Every container on every monitored server: state, image, status, CPU, memory, network. Click one to open it. |
| A container | **Shell** · **Files** · **Logs** · details · **Start / Stop / Restart** |
| **Images** | Size, age, which containers use each one; remove one, or clean up untagged ones |
| **Volumes** | Size, which containers mount each one; remove one, or all unused ones |

A server appears here once it is monitored (*Monitoring → Systems*) and its
monitoring account can run `docker` (root, or a member of the `docker` group)
or `podman`.

**Which runtime:** each monitoring reading asks both. Every container, image
and volume is tagged `docker` or `podman` (shown as a small badge), and every
action on it uses that command. A server with both shows both. Where `docker`
is Podman's compatibility shim, it is counted once, as Podman. With rootless
Podman you see the containers of the monitoring account's own user.
History charts per container are on the server's Monitoring page
([MONITORING.md](MONITORING.md#containers-docker)).

## A container

- **Shell** opens a terminal tab **inside** the container: `bash` if the image
  has it, otherwise `sh`. It is a normal timika terminal session: recorded,
  replayable, its commands logged, and listed under Sessions as
  `root → test-nginx`.
- **Files** browses the container's file system:
  - Folders first, hidden files included, with size, time and permissions.
    Links to folders (`/bin`) open like folders.
  - **Download** a file as it is, or a folder as a `.tar` (also the folder you
    are in).
  - **Upload** files, or a whole **folder** (its structure is kept), by button
    or drag and drop. Missing folders are created.
  - **New folder**, **Delete** (with confirmation; `/` is refused).
  - Volumes and bind mounts are listed on *Overview*; click a mount's path to
    open it in Files. That is how to look inside a volume.
- **Logs**: the last 100–2000 lines, with live refresh and download.
- **Overview**: image, command, id, created / started, restart policy, user,
  Compose project, published ports, networks and IPs, mounts, and environment
  (values that look like secrets are hidden until *Show values*).
- **Start / Stop / Restart**, with confirmation.

Files need a running container whose image has `sh` and `stat` (nearly all
do). A minimal image without a shell is reported as such; a folder can still
be downloaded by its path (`docker cp` needs nothing inside the image).

## How it works

| Action | Command on the server (`docker` shown; `podman` takes the same) |
|---|---|
| Shell | `docker exec -it <name> sh -c 'bash or sh'` on a PTY |
| List a folder | `docker exec <name> sh -c '… stat …' sh <path>` |
| Download a file / a folder | `docker exec <name> cat -- <path>` / `docker cp <name>:<path> -` (tar) |
| Upload | `docker exec -i <name> sh -c 'mkdir -p … && cat > "$1"' sh <path>` (the body is streamed) |
| New folder / delete | `docker exec <name> mkdir -p -- …` / `rm -rf -- …` |
| Details | `docker inspect <name>` |
| Images / volumes | `docker images`, `docker rmi`, `docker image prune -f` / `docker volume ls`, `docker volume rm`, `docker volume prune -f` |

The command is only ever the word `docker` or `podman`. Container names must
match the runtimes' own rule; every path is passed as one
quoted argument (`$1`), never pasted into a script. Downloads use a signed
link valid for one minute; uploads are limited by `FILES_MAX_UPLOAD_MB`.

## Who can do what

Everything on this page is for **administrators**: a shell, files and logs
read as the server's monitoring account, which is often root. People who
merely have access to a server see its containers and their numbers, nothing
more. Every action is in the audit trail (shells, each folder opened, uploads
and downloads with their path, removals) — never file contents.

## Limits (today)

- Docker and Podman only (no Kubernetes or containerd / nerdctl views); no Podman pods view.
- No image pull / build, no creating containers or Compose stacks from the UI.
- Files can't be edited in the browser, or copied between two containers
  directly (download, then upload).
