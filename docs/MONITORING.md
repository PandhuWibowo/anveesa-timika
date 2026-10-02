# Monitoring — your servers at a glance, with no agent

*Monitoring → Systems* shows every monitored server: up or down, CPU, memory,
disk, network, load, temperature, Docker containers, failed services, with
history and alerts. The idea comes from [Beszel](https://beszel.dev), with one
difference:

| | Beszel | timika |
|---|---|---|
| On each server | an agent to install, update and connect to the hub | **nothing**: timika already has SSH access (stored credentials, pinned host key) and reads the numbers with it |
| Network | the agent connects out to the hub (WebSocket) or the hub reaches it | timika → server over SSH, the same path as terminals |
| Secrets | a token / key per agent | none added |

The cost: a reading is one SSH command per interval instead of a push from a
local agent, so the default interval is 60 s (Beszel's agent reports more
often), and only **Linux** servers are supported.

## Start

*Systems → Monitor all N servers* (or *Add servers* to choose). That's all. The
first numbers appear within the interval. Rates such as CPU % and network need
a second reading.

Each server is read as its **first account** (any account works; no root
needed). Docker numbers need that account to be allowed to run `docker`.

## What is read

One read-only POSIX `sh` script per reading (`monitor/collect.rs`), from
`/proc`, `/sys`, `df`, and `docker` / `systemctl` when present:

| | From |
|---|---|
| CPU % | `/proc/stat` between two readings (iowait counts as idle) |
| Memory, swap | `/proc/meminfo` (used = total − available) |
| Disk usage | `df`, real file systems only; the headline number is `/` |
| Disk I/O, network | `/sys/block/*/stat`, `/proc/net/dev` (all interfaces but `lo`), as bytes per second |
| Load, uptime, OS, kernel, CPU model | `/proc/loadavg`, `/proc/uptime`, `/etc/os-release`, `/proc/cpuinfo` |
| Temperature | the hottest of `/sys/class/thermal` and `hwmon` sensors |
| Containers | `docker ps -a` and `docker stats --no-stream` (state, CPU %, memory) |
| Failed services | `systemctl --failed` |

Docker and systemd calls are limited to 8 s, so a stuck daemon doesn't make the
server look down.

**Missed readings and "down":**

- A reading that fails on the kept-open SSH connection (a firewall dropped it,
  sshd restarted) is retried once on a new connection.
- **One** missed reading is a gap in the charts and nothing more. **Two in a
  row** make the server **down**, with the reason shown ("could not connect…").
  A server that has never been read is down on its first failure ("only Linux
  servers can be monitored").
- The reason of the last missed reading stays on the server's page for a day
  ("Last missed reading 09:47 — …") and goes to the log.
- CPU, network and disk I/O are rates between two readings; after a miss they
  are computed over the longer interval, so they don't lose an extra point.
- **Gaps in the charts** are times with no reading, left empty on purpose:
  timika was restarted or sealed, its host was asleep (a laptop), or the server
  didn't answer. The last counters are stored, so a restart of timika costs
  one point, also for the rates.

## Containers (Docker)

When the monitoring account can run `docker`, each reading also takes
`docker ps -a` and `docker stats --no-stream`:

- **Per container:** state, image, status ("Up 3 hours (healthy)"), health,
  published ports, CPU %, memory (used / limit), network in / out per second.
- **History per running container** (CPU, memory, network) at the same three
  resolutions as the server's, for up to 40 containers per server. The server
  page charts the ten busiest, one line each; click a name to hide it.
- ***Monitoring → Containers*** lists every container on every monitored
  server, with filters (running · stopped · unhealthy).
- **Admins** can open a container's **logs** (the last 100–2000 lines, with a
  live refresh and download) and **start / stop / restart** it. Both run
  `docker` over SSH as the monitoring account, only for a container timika saw
  in its last reading, and both are in the audit trail. People who merely have
  access to the server see the containers and their numbers, not the logs
  (which often contain secrets) or the buttons.

## History

| Range | Points | Kept |
|---|---|---|
| 1h | every reading | ~3 hours |
| 6h · 24h | 10-minute averages | ~3 days |
| 7d · 30d · 90d | hourly averages | `MONITOR_RETENTION_DAYS` (90) |

Charts show gaps where nothing was read. Stopping monitoring a server deletes
its history; deleting the server does the same.

## Alerts

*Alerts & notifications* sets the **default rules** for every server; a
server's page can give it **its own rules** (and go back to the defaults).

| Rule | Fires when |
|---|---|
| Server is down | no answer for N minutes |
| CPU · Memory · Disk · Swap above X % | the **average over N minutes** is above X |
| Load above X, Temperature above X °C, Network in / out above X MB/s | same |

Defaults: down 2 min; CPU, memory, disk above 90 % for 10 min.

- An alert is announced **once** when it starts and once when it clears
  ("⚠ web-01: CPU 94% — above 90% for 10 min", "✓ web-01: CPU back to 41%",
  "🔴 db-01 is down for 2 min — …", "🟢 db-01 is back up").
- A rule needs readings for most (70 %) of its window before it can fire, so a
  restart of timika doesn't raise false alarms.
- **Send alerts to** Slack, Microsoft Teams, Discord, Telegram or any JSON
  webhook (`{event: "monitor", text}`), with **Send a test**. Targets are
  stored encrypted and never shown again. Set `PUBLIC_URL` for a link back.
- Firing alerts show on the list (bell badges) and on the server's page.

## Who sees what

| | Admin | Anyone with access to a server | Others |
|---|---|---|---|
| That server's numbers, charts, alerts | ✓ | ✓ | — |
| Choose what is monitored, rules, notifications | ✓ | — | — |
| Container logs, start / stop / restart | ✓ | — | — |

## Settings

| Variable | Default | |
|---|---|---|
| `MONITOR_INTERVAL_SECS` | 60 | Seconds between readings (1–3600) |
| `MONITOR_RETENTION_DAYS` | 90 | How long hourly averages are kept |
| `PUBLIC_URL` | — | Link in notifications |

## How it scales

- **One instance collects:** with Raft the leader, with Redis whoever holds
  the `monitor/lease` record (taken over within three intervals if that
  instance stops). Every instance serves what was stored.
- Servers are read 16 at a time over **kept-open SSH connections** (one per
  server), so a reading costs one command, not a login.
- Each tick is **one commit** for all servers (latest numbers + history).
  Storage per server is a few kilobytes per hour, encrypted like everything
  else.

## Limits (today)

- Linux only. No GPU, S.M.A.R.T., ZFS or battery numbers yet.
- No ping / HTTP checks from outside the server.
