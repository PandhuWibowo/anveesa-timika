# Network tools — check the network the way your servers see it

*Access → Network tools* runs a network check **from one of your servers**, over
the SSH access timika already has. That answers "can `web-1` reach the database
on 5432?", which a check from your laptop cannot. Pick several servers and the
same check runs from all of them at once, side by side.

| Tool | Like | What you get |
|---|---|---|
| **Ping** | `ping` | replies, packet loss, fastest / average / slowest |
| **Port check** | `telnet`, `nc -zv` | per port: open · refused · no answer (firewall) · unknown host, with the connect time; up to 20 ports |
| **DNS lookup** | `dig`, `nslookup` | records (A · AAAA · CNAME · MX · TXT · NS · SOA · PTR · SRV · CAA) with TTL, optionally from a resolver you name |
| **Trace route** | `traceroute`, `mtr` | the hops (at most 20), and where it stops |
| **HTTP check** | `curl` | status, and where the time goes: DNS, connect, TLS, first byte, total; redirects followed (5) |
| **TLS certificate** | `openssl s_client` | issued to / by, names, validity and days left, protocol, whether *this server* trusts it |
| **Listening ports** | `ss -tulnp` | what the server itself listens on, and the process when the account may see it |

Each result is a one-line verdict (green, amber or red), the key numbers, and
the program's output as it came, folded underneath. Results stay for your visit
(memory only); **Run again** repeats one.

## What runs on the server

One `sh` script per check, using what the server has — and saying so when it has
nothing:

| Tool | Uses, in this order |
|---|---|
| Ping | `ping -c 4 -W 2` |
| Port check | `bash` (`/dev/tcp`, 3 s) · `nc -z -w 3` |
| DNS lookup | `dig` · `host` · `nslookup` · `getent ahosts` |
| Trace route | `traceroute` · `tracepath` · `mtr` |
| HTTP check | `curl` (http and https only) · `wget --spider` |
| TLS certificate | `openssl s_client` + `openssl x509` (SNI for names, 10 s) |
| Listening ports | `ss` · `netstat` |

## Safety

- **No free-form command.** The tool is one of the seven. A target must be a
  hostname or IP address (letters, digits, `.` `-` `:` `_`, not starting with
  `-`), ports are numbers, the record type is from the list, a URL must be
  `http(s)://` without `user:password@`. Every value reaches the server as one
  single-quoted word.
- **Bounded:** fixed counts and timeouts per tool, at most 64 KiB of output.
- **Only where you have access:** the check runs as an account you could open a
  terminal with (the first you may use, or the one you name). No grant, no check.
- **Nothing runs on the timika host** — the vault is not a way to scan its own
  network.
- **Audited:** `NetService/Run` with the tool, target and `account@server`. A
  URL's query string is neither logged nor echoed back.

## API

`NetService.Run(tool, asset, account?, target, ports?, record?, resolver?)` →
`NetResult` (`ok`, `level`, `verdict`, `tool`, `output`, `took_ms`, and `facts`,
`ports`, `records`, `hops` or `listeners` depending on the tool).

## Limits (today)

- Linux servers. The account's own rights apply: `ping` may need the usual
  capability, and processes of other users show only to root.
- One-off checks; nothing is scheduled or alerted on yet.
- Trace route's "reached" is inferred from the trace ending before 20 hops.
