//! Network checks run from a server over SSH (docs/NETWORK.md): ping, port
//! check, DNS, trace route, HTTP, TLS certificate, listening ports.
//!
//! There is no free-form command. The tool is one of a fixed set, every
//! argument is validated (hostnames, ports, record types) and then passed as
//! one quoted word, and each run is bounded (counts, timeouts, output size).

use chrono::{NaiveDateTime, Utc};

use crate::bastion::archive::q;
use crate::error::{AppError, AppResult};
use crate::grpc::pb;

pub const MAX_OUTPUT: usize = 64 * 1024;
const MAX_PORTS: usize = 20;
const MAX_HOPS: usize = 20;
const RECORDS: [&str; 10] = ["A", "AAAA", "CNAME", "MX", "TXT", "NS", "SOA", "PTR", "SRV", "CAA"];

const PING: &str = r##"H=@H@
command -v ping >/dev/null 2>&1 || { echo '#missing=ping'; exit 127; }
echo '#tool=ping'
ping -c 4 -W 2 "$H" 2>&1"##;

// bash's /dev/tcp gives the same answers everywhere; nc is the fallback.
const PORT: &str = r##"H=@H@
now() { T=$(date +%s%N 2>/dev/null); case $T in *[!0-9]*|'') echo;; *) echo "$T";; esac; }
if command -v bash >/dev/null 2>&1 && command -v timeout >/dev/null 2>&1; then M=bash
elif command -v nc >/dev/null 2>&1; then M=nc
else echo '#missing=bash (with timeout) or nc'; exit 127; fi
echo "#tool=$M"
for P in @PORTS@; do
  S=$(now)
  if [ "$M" = bash ]; then OUT=$(timeout 3 bash -c 'exec 3<>"/dev/tcp/$0/$1"' "$H" "$P" 2>&1); RC=$?
  else OUT=$(nc -z -w 3 "$H" "$P" 2>&1); RC=$?; fi
  E=$(now); MS=
  [ -n "$S" ] && [ -n "$E" ] && MS=$(( (E - S) / 1000000 ))
  echo "port=$P|$RC|$MS|$(echo "$OUT" | tr '\n' ' ')"
done
exit 0"##;

const DNS: &str = r##"N=@N@; T=@T@; R=@R@
if command -v dig >/dev/null 2>&1; then
  echo '#tool=dig'
  if [ "$T" = PTR ]; then set -- -x "$N"; else set -- "$N" "$T"; fi
  dig +noall +comments +answer +stats +time=3 +tries=1 ${R:+"@$R"} "$@" 2>&1
elif command -v host >/dev/null 2>&1; then
  echo '#tool=host'
  if [ "$T" = PTR ]; then host -W 3 "$N" ${R:+"$R"} 2>&1; else host -W 3 -t "$T" "$N" ${R:+"$R"} 2>&1; fi
elif command -v nslookup >/dev/null 2>&1; then
  echo '#tool=nslookup'
  nslookup -type="$T" "$N" ${R:+"$R"} 2>&1
elif command -v getent >/dev/null 2>&1; then
  echo '#tool=getent'
  getent ahosts "$N" 2>&1
else echo '#missing=dig, host, nslookup or getent'; exit 127; fi"##;

const TRACE: &str = r##"H=@H@
if command -v traceroute >/dev/null 2>&1; then echo '#tool=traceroute'; traceroute -n -w 2 -q 1 -m @MAX@ "$H" 2>&1
elif command -v tracepath >/dev/null 2>&1; then echo '#tool=tracepath'; tracepath -n -m @MAX@ "$H" 2>&1
elif command -v mtr >/dev/null 2>&1; then echo '#tool=mtr'; mtr -n -r -c 1 -m @MAX@ "$H" 2>&1
else echo '#missing=traceroute, tracepath or mtr'; exit 127; fi"##;

const HTTP: &str = r##"U=@U@
if command -v curl >/dev/null 2>&1; then
  echo '#tool=curl'
  curl -sS -o /dev/null -L --max-redirs 5 --connect-timeout 5 --max-time 20 --proto '=http,https' --proto-redir '=http,https' -w 'code=%{http_code}\nip=%{remote_ip}\ndns=%{time_namelookup}\nconnect=%{time_connect}\ntls=%{time_appconnect}\nfirst=%{time_starttransfer}\ntotal=%{time_total}\nredirects=%{num_redirects}\nurl=%{url_effective}\nsize=%{size_download}\n' "$U" 2>&1
elif command -v wget >/dev/null 2>&1; then
  echo '#tool=wget'
  wget -S --spider -T 10 -t 1 "$U" 2>&1
else echo '#missing=curl or wget'; exit 127; fi"##;

const TLS: &str = r##"C=@C@; S=@S@
command -v openssl >/dev/null 2>&1 || { echo '#missing=openssl'; exit 127; }
echo '#tool=openssl'
TO=; command -v timeout >/dev/null 2>&1 && TO='timeout 10'
O=$(echo | $TO openssl s_client -connect "$C" ${S:+-servername "$S"} 2>&1)
echo "$O" | grep -E '^(New, |Verification|verify error| *Verify return code| *Protocol *:|connect:)|errno|refused|timed out|not known'
echo "$O" | openssl x509 -noout -subject -issuer -startdate -enddate -ext subjectAltName 2>&1
exit 0"##;

const LISTEN: &str = r##"if command -v ss >/dev/null 2>&1; then echo '#tool=ss'; ss -tulnp 2>&1
elif command -v netstat >/dev/null 2>&1; then echo '#tool=netstat'; netstat -tulnp 2>&1
else echo '#missing=ss or netstat'; exit 127; fi"##;

/// A hostname or IP address: nothing a shell or a program reads as an option.
pub fn host(h: &str) -> AppResult<String> {
    let h = h.trim();
    let ok = !h.is_empty()
        && h.len() <= 253
        && !h.starts_with('-')
        && h.chars().any(|c| c.is_ascii_alphanumeric())
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'));
    if ok { Ok(h.to_string()) } else { Err(AppError::BadRequest("the target must be a hostname or an IP address".into())) }
}

/// An http(s) URL without credentials in it (https:// when no scheme is given).
pub fn url(u: &str) -> AppResult<String> {
    let u = u.trim();
    let u = if u.contains("://") { u.to_string() } else { format!("https://{u}") };
    let rest = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")).ok_or_else(|| AppError::BadRequest("only http:// and https:// URLs".into()))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let ok = u.len() <= 2000
        && !authority.is_empty()
        && !authority.starts_with('-')
        && authority.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_' | '[' | ']'))
        && u.chars().all(|c| c.is_ascii_graphic());
    if ok { Ok(u) } else { Err(AppError::BadRequest("that is not a URL this check can open (no spaces, no user:password@)".into())) }
}

#[derive(Debug, PartialEq)]
pub enum Check {
    Ping(String),
    Port(String, Vec<u16>),
    Dns { name: String, record: String, resolver: String },
    Trace(String),
    Http(String),
    Tls(String, u16),
    Listen,
}

fn port(p: u32) -> AppResult<u16> {
    u16::try_from(p).ok().filter(|p| *p > 0).ok_or_else(|| AppError::BadRequest(format!("{p} is not a port (1–65535)")))
}

impl Check {
    pub fn new(r: &pb::NetRequest) -> AppResult<Self> {
        Ok(match r.tool.as_str() {
            "ping" => Check::Ping(host(&r.target)?),
            "trace" => Check::Trace(host(&r.target)?),
            "port" => {
                let mut ports = r.ports.iter().map(|p| port(*p)).collect::<AppResult<Vec<u16>>>()?;
                ports.dedup();
                if ports.is_empty() || ports.len() > MAX_PORTS {
                    return Err(AppError::BadRequest(format!("give 1 to {MAX_PORTS} ports")));
                }
                Check::Port(host(&r.target)?, ports)
            }
            "dns" => {
                let record = if r.record.is_empty() { "A".to_string() } else { r.record.to_uppercase() };
                if !RECORDS.contains(&record.as_str()) {
                    return Err(AppError::BadRequest(format!("unknown record type `{}` ({})", r.record, RECORDS.join(" · "))));
                }
                let resolver = if r.resolver.trim().is_empty() { String::new() } else { host(&r.resolver).map_err(|_| AppError::BadRequest("the resolver must be a hostname or an IP address".into()))? };
                Check::Dns { name: host(&r.target)?, record, resolver }
            }
            "http" => Check::Http(url(&r.target)?),
            "tls" => {
                if r.ports.len() > 1 {
                    return Err(AppError::BadRequest("one port for a certificate check".into()));
                }
                Check::Tls(host(&r.target)?, r.ports.first().map(|p| port(*p)).transpose()?.unwrap_or(443))
            }
            "listen" => Check::Listen,
            other => return Err(AppError::BadRequest(format!("unknown tool `{other}` (ping · port · dns · trace · http · tls · listen)"))),
        })
    }

    /// For the audit log: what was checked — never a URL's query string.
    pub fn describe(&self) -> String {
        match self {
            Check::Ping(h) => format!("ping {h}"),
            Check::Port(h, p) => format!("port check {h}:{}", p.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",")),
            Check::Dns { name, record, resolver } => format!("dns {record} {name}{}", if resolver.is_empty() { String::new() } else { format!(" @{resolver}") }),
            Check::Trace(h) => format!("trace route to {h}"),
            Check::Http(u) => format!("http check {}", u.split(['?', '#']).next().unwrap_or_default()),
            Check::Tls(h, p) => format!("tls certificate of {h}:{p}"),
            Check::Listen => "listening ports".into(),
        }
    }

    /// Seconds the whole run may take.
    pub fn secs(&self) -> u64 {
        match self {
            Check::Port(_, p) => 10 + 4 * p.len() as u64,
            Check::Trace(_) => 70,
            Check::Http(_) => 30,
            Check::Tls(..) => 25,
            _ => 20,
        }
    }

    fn script(&self) -> String {
        match self {
            Check::Ping(h) => PING.replace("@H@", &q(h)),
            Check::Port(h, p) => PORT.replace("@H@", &q(h)).replace("@PORTS@", &p.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ")),
            Check::Dns { name, record, resolver } => DNS.replace("@N@", &q(name)).replace("@T@", record).replace("@R@", &q(resolver)),
            Check::Trace(h) => TRACE.replace("@H@", &q(h)).replace("@MAX@", &MAX_HOPS.to_string()),
            Check::Http(u) => HTTP.replace("@U@", &q(u)),
            Check::Tls(h, p) => {
                let ip = h.parse::<std::net::IpAddr>().is_ok();
                let connect = if h.contains(':') { format!("[{h}]:{p}") } else { format!("{h}:{p}") };
                // No SNI for an IP address.
                TLS.replace("@C@", &q(&connect)).replace("@S@", &q(if ip { "" } else { h }))
            }
            Check::Listen => LISTEN.to_string(),
        }
    }

    /// The command for the server: `sh`, whatever the account's login shell is.
    pub fn command(&self) -> String {
        format!("cd -- '.' && sh -c {}", q(&self.script()))
    }

    pub fn parse(&self, code: u32, raw: &str) -> pb::NetResult {
        let mut r = pb::NetResult::default();
        let mut lines = Vec::new();
        let mut missing = None;
        for l in raw.lines() {
            if let Some(t) = l.strip_prefix("#tool=") {
                r.tool = t.trim().to_string();
            } else if let Some(m) = l.strip_prefix("#missing=") {
                missing = Some(m.trim().to_string());
            } else {
                lines.push(l);
            }
        }
        r.output = lines.join("\n").trim_end().to_string();
        if let Some(m) = missing {
            return verdict(r, "bad", format!("{m} is not installed on this server"));
        }
        match self {
            Check::Ping(h) => ping(r, h, &lines),
            Check::Port(h, _) => ports(r, h, &lines),
            Check::Dns { name, record, .. } => dns(r, name, record, code, &lines),
            Check::Trace(h) => trace(r, h, &lines),
            Check::Http(_) => http(r, &lines),
            Check::Tls(h, p) => tls(r, h, *p, &lines),
            Check::Listen => listen(r, &lines),
        }
    }
}

fn verdict(mut r: pb::NetResult, level: &str, text: String) -> pb::NetResult {
    r.ok = level != "bad";
    r.level = level.into();
    r.verdict = text;
    r
}

fn fact(label: &str, value: impl Into<String>, level: &str) -> pb::NetFact {
    pb::NetFact { label: label.into(), value: value.into(), level: level.into() }
}

/// The first line that says something, for when a tool only printed an error.
fn first_line(lines: &[&str]) -> String {
    lines.iter().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("no output").chars().take(200).collect()
}

fn leading_number(s: &str) -> Option<f64> {
    let s = s.trim();
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    s[..end].parse().ok()
}

fn ms(v: f64) -> String {
    if v < 10.0 { format!("{v:.1} ms") } else { format!("{v:.0} ms") }
}

fn ping(mut r: pb::NetResult, h: &str, lines: &[&str]) -> pb::NetResult {
    let Some(stats) = lines.iter().find(|l| l.contains("packets transmitted")) else {
        return verdict(r, "bad", first_line(lines));
    };
    let parts: Vec<&str> = stats.split(',').collect();
    let sent = parts.first().and_then(|p| leading_number(p)).unwrap_or(0.0);
    let got = parts.get(1).and_then(|p| leading_number(p)).unwrap_or(0.0);
    let loss = parts.iter().find(|p| p.contains("packet loss")).and_then(|p| leading_number(p)).unwrap_or(if sent > 0.0 { (sent - got) / sent * 100.0 } else { 100.0 });
    r.facts.push(fact("Replies", format!("{got:.0} of {sent:.0}"), ""));
    r.facts.push(fact("Packet loss", format!("{loss:.0}%"), if loss >= 100.0 { "bad" } else if loss > 0.0 { "warn" } else { "ok" }));
    // "rtt min/avg/max/mdev = 0.031/0.040/0.052/0.008 ms" · busybox: "round-trip min/avg/max = …"
    let rtt: Vec<f64> = lines.iter().find(|l| l.contains("min/avg/max")).and_then(|l| l.split('=').nth(1)).map(|v| v.split('/').filter_map(leading_number).collect()).unwrap_or_default();
    if let [min, avg, max, ..] = rtt[..] {
        r.facts.push(fact("Fastest", ms(min), ""));
        r.facts.push(fact("Average", ms(avg), ""));
        r.facts.push(fact("Slowest", ms(max), ""));
    }
    let avg = rtt.get(1).map(|a| format!(", {} on average", ms(*a))).unwrap_or_default();
    if got == 0.0 {
        verdict(r, "bad", format!("{h} did not answer"))
    } else if loss > 0.0 {
        verdict(r, "warn", format!("{h} answers, but {loss:.0}% of the packets were lost{avg}"))
    } else {
        verdict(r, "ok", format!("{h} answers{avg}"))
    }
}

fn ports(mut r: pb::NetResult, h: &str, lines: &[&str]) -> pb::NetResult {
    for l in lines {
        let Some(f) = l.strip_prefix("port=") else { continue };
        let f: Vec<&str> = f.splitn(4, '|').collect();
        let (Some(port), Some(rc)) = (f.first().and_then(|p| p.parse().ok()), f.get(1).and_then(|c| c.parse::<u32>().ok())) else { continue };
        let detail = f.get(3).map(|d| d.trim()).unwrap_or_default();
        let low = detail.to_lowercase();
        let state = if rc == 0 {
            "open"
        } else if low.contains("refused") {
            "closed"
        } else if rc == 124 || low.contains("timed out") || low.contains("timeout") || f.get(2).and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| m >= 2900) {
            "timeout"
        } else if low.contains("not known") || low.contains("resolve") || low.contains("nodename") {
            "unresolved"
        } else {
            "failed"
        };
        r.ports.push(pb::PortResult { port, state: state.into(), ms: f.get(2).and_then(|m| m.parse().ok()), detail: detail.chars().take(200).collect() });
    }
    if r.ports.is_empty() {
        return verdict(r, "bad", first_line(lines));
    }
    let open = r.ports.iter().filter(|p| p.state == "open").count();
    let text = match (&r.ports[..], open) {
        ([p], _) => match p.state.as_str() {
            "open" => format!("{h}:{} is open", p.port),
            "closed" => format!("{h}:{} refused the connection — nothing listens there", p.port),
            "timeout" => format!("{h}:{} did not answer — a firewall drops it, or the host is down", p.port),
            "unresolved" => format!("{h} could not be resolved"),
            _ => format!("{h}:{} could not be reached", p.port),
        },
        (all, n) => format!("{n} of {} ports open on {h}", all.len()),
    };
    let level = if open == r.ports.len() { "ok" } else if open == 0 { "bad" } else { "warn" };
    verdict(r, level, text)
}

fn record(name: &str, ttl: Option<u32>, kind: &str, value: &str) -> pb::DnsRecord {
    pb::DnsRecord { name: name.trim_end_matches('.').into(), ttl, r#type: kind.into(), value: value.trim().into() }
}

fn dns(mut r: pb::NetResult, name: &str, kind: &str, code: u32, lines: &[&str]) -> pb::NetResult {
    let mut status = String::new();
    match r.tool.as_str() {
        "dig" => {
            for l in lines {
                if let Some(s) = l.split("status: ").nth(1) {
                    status = s.split(',').next().unwrap_or_default().to_string();
                } else if let Some(t) = l.strip_prefix(";; Query time:") {
                    r.facts.push(fact("Answered in", t.trim().replace("msec", "ms"), ""));
                } else if let Some(s) = l.strip_prefix(";; SERVER:") {
                    r.facts.push(fact("Resolver", s.trim().split('(').next().unwrap_or_default(), ""));
                } else if !l.starts_with(';') && !l.trim().is_empty() {
                    // name  ttl  IN  TYPE  value…
                    let f: Vec<&str> = l.split_whitespace().collect();
                    if f.len() >= 5 && f[2] == "IN" {
                        r.records.push(record(f[0], f[1].parse().ok(), f[3], &f[4..].join(" ")));
                    }
                }
            }
        }
        "host" => {
            for l in lines {
                for (mark, t) in [(" has address ", "A"), (" has IPv6 address ", "AAAA"), (" mail is handled by ", "MX"), (" is an alias for ", "CNAME"), (" name server ", "NS"), (" descriptive text ", "TXT"), (" domain name pointer ", "PTR"), (" has SOA record ", "SOA")] {
                    if let Some((n, v)) = l.split_once(mark) {
                        r.records.push(record(n, None, t, v.trim_end_matches('.')));
                    }
                }
                if let Some(s) = l.split("not found: ").nth(1) {
                    status = s.split('(').nth(1).unwrap_or(s).trim_end_matches(')').to_string();
                }
            }
        }
        "nslookup" => {
            // Addresses after the "Name:" line are the answer; the ones before are the resolver.
            let mut answer = false;
            for l in lines {
                if l.starts_with("Name:") {
                    answer = true;
                } else if let Some(a) = l.strip_prefix("Address").filter(|_| answer) {
                    let a = a.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ').trim_start_matches(':').trim();
                    r.records.push(record(name, None, if a.contains(':') { "AAAA" } else { "A" }, a));
                } else if l.contains("NXDOMAIN") {
                    status = "NXDOMAIN".into();
                }
            }
        }
        _ => {
            for l in lines {
                if let Some(a) = l.split_whitespace().next().filter(|a| a.parse::<std::net::IpAddr>().is_ok()) {
                    if !r.records.iter().any(|x| x.value == a) {
                        r.records.push(record(name, None, if a.contains(':') { "AAAA" } else { "A" }, a));
                    }
                }
            }
        }
    }
    if !r.records.is_empty() {
        let n = r.records.len();
        let first = r.records.iter().find(|x| x.r#type == kind).unwrap_or(&r.records[0]).value.clone();
        return verdict(r, "ok", if n == 1 { format!("{name} → {first}") } else { format!("{name} → {first} and {} more", n - 1) });
    }
    let text = match status.as_str() {
        "NXDOMAIN" => format!("{name} does not exist (NXDOMAIN)"),
        "NOERROR" => format!("{name} exists, but has no {kind} record"),
        "" if r.tool != "dig" && code == 0 && r.tool != "getent" => format!("{name}: no {kind} record in the answer"),
        "" => format!("{name}: {}", lines.iter().map(|l| l.trim()).find(|l| !l.is_empty()).map(|l| l.trim_start_matches(";; ").to_string()).unwrap_or_else(|| "no answer".into())),
        s => format!("{name}: the resolver answered {s}"),
    };
    verdict(r, "bad", text)
}

fn trace(mut r: pb::NetResult, h: &str, lines: &[&str]) -> pb::NetResult {
    for l in lines {
        let f: Vec<&str> = l.split_whitespace().collect();
        // "1  10.0.0.1  0.4 ms" · "1:  10.0.0.1  0.4ms" · "1?: [LOCALHOST]" · "1.|-- 10.0.0.1 0.0% 1 0.4 …"
        let Some(n) = f.first().and_then(|t| t.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<u32>().ok()) else { continue };
        if f[0].contains('?') || n == 0 || n as usize > MAX_HOPS {
            continue;
        }
        let addr = f.iter().skip(1).find(|t| t.parse::<std::net::IpAddr>().is_ok()).copied().unwrap_or_default();
        let time = f.iter().enumerate().skip(1).find_map(|(i, t)| if *t == "ms" { f[i - 1].parse().ok() } else { t.strip_suffix("ms").and_then(|v| v.parse().ok()) });
        match r.hops.iter_mut().find(|x| x.n == n) {
            // tracepath repeats a hop; keep the line that answered.
            Some(x) if x.addr.is_empty() => *x = pb::Hop { n, addr: addr.into(), ms: time },
            Some(_) => {}
            None => r.hops.push(pb::Hop { n, addr: addr.into(), ms: time }),
        }
    }
    let Some(last) = r.hops.last().cloned() else {
        return verdict(r, "bad", first_line(lines));
    };
    // A trace ends early only when the destination answered.
    if !last.addr.is_empty() && (r.hops.len() < MAX_HOPS || lines.iter().any(|l| l.contains("Resume:") && l.contains("reached"))) {
        let t = last.ms.map(|v| format!(", {}", ms(v))).unwrap_or_default();
        verdict(r, "ok", format!("{h} reached in {} hops{t}", last.n))
    } else {
        let answered = r.hops.iter().rev().find(|x| !x.addr.is_empty()).map(|x| format!("the last answer came from {} (hop {})", x.addr, x.n)).unwrap_or_else(|| "no hop answered".into());
        verdict(r, "warn", format!("{h} not reached within {MAX_HOPS} hops — {answered}"))
    }
}

fn http(mut r: pb::NetResult, lines: &[&str]) -> pb::NetResult {
    if r.tool == "wget" {
        let code = lines.iter().rev().find_map(|l| l.trim().strip_prefix("HTTP/").and_then(|x| x.split_whitespace().nth(1)).and_then(|c| c.parse::<u32>().ok()));
        return match code {
            Some(c) => {
                r.facts.push(fact("Status", c.to_string(), if c < 400 { "ok" } else { "bad" }));
                verdict(r, if c < 400 { "ok" } else { "bad" }, format!("HTTP {c}"))
            }
            None => verdict(r, "bad", lines.iter().rev().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("no answer").chars().take(200).collect()),
        };
    }
    let get = |k: &str| lines.iter().find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix('='))).unwrap_or_default().trim();
    let secs = |k: &str| get(k).parse::<f64>().unwrap_or(0.0) * 1000.0;
    let code: u32 = get("code").parse().unwrap_or(0);
    let total = secs("total");
    if code == 0 {
        // curl's own words: "curl: (7) Failed to connect to …"
        let why = lines.iter().find(|l| l.starts_with("curl:")).map(|l| l.splitn(2, ") ").last().unwrap_or(l).to_string()).unwrap_or_else(|| first_line(lines));
        return verdict(r, "bad", why);
    }
    let level = if code < 400 { "ok" } else { "bad" };
    r.facts.push(fact("Status", code.to_string(), level));
    if !get("ip").is_empty() {
        r.facts.push(fact("Answered by", get("ip"), ""));
    }
    let (dns, connect, tls, first) = (secs("dns"), secs("connect"), secs("tls"), secs("first"));
    r.facts.push(fact("DNS", ms(dns), ""));
    r.facts.push(fact("Connect", ms((connect - dns).max(0.0)), ""));
    if tls > 0.0 {
        r.facts.push(fact("TLS", ms((tls - connect).max(0.0)), ""));
    }
    r.facts.push(fact("First byte", ms((first - tls.max(connect)).max(0.0)), ""));
    r.facts.push(fact("Total", ms(total), ""));
    let redirects: u32 = get("redirects").parse().unwrap_or(0);
    if redirects > 0 {
        r.facts.push(fact("Redirects", format!("{redirects} → {}", get("url").split(['?', '#']).next().unwrap_or_default()), ""));
    }
    // Only the numbers: the effective URL may carry a query string.
    r.output = lines.iter().filter(|l| !l.starts_with("url=")).copied().collect::<Vec<_>>().join("\n");
    verdict(r, level, format!("HTTP {code} in {}", ms(total)))
}

fn tls(mut r: pb::NetResult, h: &str, port: u16, lines: &[&str]) -> pb::NetResult {
    let get = |k: &str| lines.iter().find_map(|l| l.trim().strip_prefix(k)).map(|v| v.trim().to_string());
    let date = |v: Option<String>| v.and_then(|d| NaiveDateTime::parse_from_str(&d.trim_end_matches("GMT").split_whitespace().collect::<Vec<_>>().join(" "), "%b %d %H:%M:%S %Y").ok());
    let Some(until) = date(get("notAfter=")) else {
        let all = lines.join("\n").to_lowercase();
        let why = if all.contains("refused") || all.contains("errno=111") {
            "the connection was refused"
        } else if all.contains("timed out") || all.contains("errno=110") {
            "the connection timed out"
        } else if all.contains("not known") || all.contains("resolve") {
            "the name could not be resolved"
        } else {
            "the server did not present one (is it a TLS port?)"
        };
        return verdict(r, "bad", format!("no certificate from {h}:{port} — {why}"));
    };
    let days = (until - Utc::now().naive_utc()).num_days();
    // "Verify return code: 0 (ok)" · OpenSSL 3: "Verification: OK" / "Verification error: …"
    let trust = match (get("Verify return code:"), get("Verification error:"), get("Verification:")) {
        (Some(c), _, _) if c.starts_with("0 ") => None,
        (Some(c), _, _) => Some(c.split_once('(').map(|x| x.1.trim_end_matches(')').to_string()).unwrap_or(c)),
        (_, Some(e), _) => Some(e),
        _ => None,
    };
    let subject = get("subject=").unwrap_or_default();
    let issuer = get("issuer=").unwrap_or_default();
    // The readable part of "C = US, O = Let's Encrypt, CN = R11".
    let short = |dn: &str| dn.rsplit([',', '/']).find_map(|p| p.trim().strip_prefix("CN").map(|v| v.trim_start_matches([' ', '=']).to_string())).unwrap_or_else(|| dn.to_string());
    let names: Vec<String> = lines.iter().filter(|l| l.contains("DNS:") || l.contains("IP Address:")).flat_map(|l| l.split(',')).map(|n| n.trim().trim_start_matches("DNS:").trim_start_matches("IP Address:").to_string()).collect();
    r.facts.push(fact("Issued to", short(&subject), ""));
    r.facts.push(fact("Issued by", short(&issuer), ""));
    if !names.is_empty() {
        let more = names.len().saturating_sub(6);
        r.facts.push(fact("Names", format!("{}{}", names.iter().take(6).cloned().collect::<Vec<_>>().join(", "), if more > 0 { format!(" +{more} more") } else { String::new() }), ""));
    }
    if let Some(from) = date(get("notBefore=")) {
        r.facts.push(fact("Valid from", from.format("%Y-%m-%d").to_string(), ""));
    }
    let left = if days < 0 { format!("expired {} days ago", -days) } else { format!("{days} days left") };
    r.facts.push(fact("Valid until", format!("{} · {left}", until.format("%Y-%m-%d")), if days < 0 { "bad" } else if days < 21 { "warn" } else { "ok" }));
    if let Some(p) = get("New, ") {
        r.facts.push(fact("Protocol", p.replace(", Cipher is ", " · "), ""));
    }
    r.facts.push(fact("Trusted by this server", trust.clone().unwrap_or_else(|| "yes".into()), if trust.is_some() { "bad" } else { "ok" }));
    if days < 0 {
        verdict(r, "bad", format!("the certificate of {h}:{port} expired {} days ago", -days))
    } else if let Some(t) = trust {
        verdict(r, "bad", format!("{h}:{port} is not trusted from this server: {t}"))
    } else if days < 21 {
        verdict(r, "warn", format!("valid, but expires in {days} days"))
    } else {
        verdict(r, "ok", format!("valid for {days} more days, issued by {}", short(&issuer)))
    }
}

fn listen(mut r: pb::NetResult, lines: &[&str]) -> pb::NetResult {
    for l in lines {
        let f: Vec<&str> = l.split_whitespace().collect();
        let proto = f.first().copied().unwrap_or_default();
        if !(proto.starts_with("tcp") || proto.starts_with("udp")) {
            continue;
        }
        // ss: proto state recv send local peer [users:(("nginx",pid=812,fd=6))]
        // netstat: proto recv send local peer [LISTEN] [812/nginx]
        let (local, owner) = if r.tool == "netstat" { (f.get(3), f.last().filter(|t| t.contains('/'))) } else { (f.get(4), f.get(6)) };
        let Some((addr, port)) = local.and_then(|a| a.rsplit_once(':')).and_then(|(a, p)| Some((a, p.parse::<u32>().ok()?))) else { continue };
        let (process, pid) = match owner {
            Some(o) if r.tool == "netstat" => (o.split_once('/').map(|x| x.1).unwrap_or_default().to_string(), o.split('/').next().and_then(|p| p.parse().ok())),
            Some(o) => (o.split('"').nth(1).unwrap_or_default().to_string(), o.split("pid=").nth(1).and_then(|p| p.split(|c: char| !c.is_ascii_digit()).next()).and_then(|p| p.parse().ok())),
            None => (String::new(), None),
        };
        let item = pb::Listener { proto: proto.chars().take(3).collect(), addr: addr.trim_matches(['[', ']']).to_string(), port, process, pid };
        if !r.listeners.contains(&item) {
            r.listeners.push(item);
        }
    }
    if r.listeners.is_empty() {
        return verdict(r, if lines.len() <= 1 { "warn" } else { "bad" }, if lines.len() <= 1 { "nothing is listening".into() } else { first_line(lines) });
    }
    r.listeners.sort_by(|a, b| (a.port, &a.proto, &a.addr).cmp(&(b.port, &b.proto, &b.addr)));
    let open = r.listeners.iter().filter(|x| !matches!(x.addr.as_str(), "127.0.0.1" | "::1") && !x.addr.starts_with("127.")).map(|x| x.port).collect::<std::collections::BTreeSet<_>>().len();
    let total = r.listeners.iter().map(|x| (x.port, x.proto.clone())).collect::<std::collections::BTreeSet<_>>().len();
    verdict(r, "ok", format!("{total} listening ports, {open} reachable from other machines"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(tool: &str, target: &str) -> pb::NetRequest {
        pb::NetRequest { tool: tool.into(), target: target.into(), ..Default::default() }
    }

    #[test]
    fn only_hostnames_ports_and_known_tools() {
        for bad in ["", " ", "-c", "a b", "a;id", "$(id)", "a`id`", "a'b", "a|b", "..", "a/b", &"a".repeat(300)] {
            assert!(host(bad).is_err(), "{bad:?}");
        }
        for good in ["example.com", "10.0.0.1", "2001:db8::1", "db_1.internal", " web-1 "] {
            assert!(host(good).is_ok(), "{good:?}");
        }
        assert_eq!(url("example.com/health").unwrap(), "https://example.com/health");
        assert_eq!(url("http://10.0.0.1:8080/a?b=c").unwrap(), "http://10.0.0.1:8080/a?b=c");
        for bad in ["ftp://x", "file:///etc/passwd", "https://user:pw@x", "https://", "https://a b", "https://-x", "https://x/'$(id)' y"] {
            assert!(url(bad).is_err(), "{bad:?}");
        }
        assert!(Check::new(&req("sh", "x")).is_err());
        assert!(Check::new(&pb::NetRequest { ports: vec![0], ..req("port", "x") }).is_err());
        assert!(Check::new(&pb::NetRequest { ports: vec![70000], ..req("port", "x") }).is_err());
        assert!(Check::new(&pb::NetRequest { ports: (1..=21).collect(), ..req("port", "x") }).is_err());
        assert!(Check::new(&req("port", "x")).is_err(), "a port is needed");
        assert!(Check::new(&pb::NetRequest { record: "ANY; id".into(), ..req("dns", "x") }).is_err());
        assert!(Check::new(&pb::NetRequest { resolver: "-x".into(), ..req("dns", "x") }).is_err());
        assert_eq!(Check::new(&req("tls", "example.com")).unwrap(), Check::Tls("example.com".into(), 443));
        assert_eq!(Check::new(&req("listen", "")).unwrap(), Check::Listen);
    }

    #[test]
    fn arguments_are_single_quoted_words() {
        let c = Check::new(&pb::NetRequest { ports: vec![22, 443], ..req("port", "db.internal") }).unwrap();
        let cmd = c.command();
        assert!(cmd.starts_with("cd -- '.' && sh -c '"), "{cmd}");
        assert!(cmd.contains("for P in 22 443; do"), "{cmd}");
        assert_eq!(Check::Http("https://x/?token=abc".into()).describe(), "http check https://x/", "no query string in the audit log");
        assert!(Check::Tls("2001:db8::1".into(), 8443).script().contains("C='[2001:db8::1]:8443'; S=''"), "IPv6 in brackets, no SNI for an address");
    }

    #[test]
    fn ping_results() {
        let c = Check::Ping("db".into());
        let r = c.parse(0, "#tool=ping\nPING db (10.0.0.5) 56(84) bytes of data.\n64 bytes from 10.0.0.5: icmp_seq=1 ttl=64 time=0.4 ms\n\n--- db ping statistics ---\n4 packets transmitted, 4 received, 0% packet loss, time 3004ms\nrtt min/avg/max/mdev = 0.310/0.402/0.520/0.080 ms\n");
        assert_eq!((r.ok, r.level.as_str(), r.verdict.as_str(), r.tool.as_str()), (true, "ok", "db answers, 0.4 ms on average", "ping"));
        assert!(!r.output.contains("#tool"));
        let r = c.parse(1, "#tool=ping\n4 packets transmitted, 0 received, 100% packet loss, time 3000ms\n");
        assert_eq!((r.ok, r.verdict.as_str()), (false, "db did not answer"));
        let r = c.parse(0, "#tool=ping\n4 packets transmitted, 3 packets received, 25% packet loss\nround-trip min/avg/max = 10.0/12.5/20.0 ms\n");
        assert_eq!((r.level.as_str(), r.verdict.as_str()), ("warn", "db answers, but 25% of the packets were lost, 12 ms on average"));
        let r = c.parse(2, "#tool=ping\nping: db: Name or service not known\n");
        assert_eq!((r.ok, r.verdict.as_str()), (false, "ping: db: Name or service not known"));
        let r = c.parse(127, "#missing=ping\n");
        assert_eq!((r.ok, r.verdict.as_str()), (false, "ping is not installed on this server"));
    }

    #[test]
    fn port_results() {
        let c = Check::Port("db".into(), vec![22, 81, 82]);
        let r = c.parse(0, "#tool=bash\nport=22|0|3|\nport=81|1|1|bash: connect: Connection refused bash: line 1: /dev/tcp/db/81: Connection refused \nport=82|124|3001|\n");
        assert_eq!(r.ports.iter().map(|p| (p.port, p.state.as_str(), p.ms)).collect::<Vec<_>>(), [(22, "open", Some(3)), (81, "closed", Some(1)), (82, "timeout", Some(3001))]);
        assert_eq!((r.level.as_str(), r.verdict.as_str()), ("warn", "1 of 3 ports open on db"));
        let r = Check::Port("db".into(), vec![81]).parse(0, "#tool=nc\nport=81|1||Ncat: Connection refused. \n");
        assert_eq!((r.ok, r.verdict.as_str(), r.ports[0].ms), (false, "db:81 refused the connection — nothing listens there", None));
        let r = Check::Port("db".into(), vec![81]).parse(0, "#tool=nc\nport=81|1|3004|\n");
        assert_eq!(r.ports[0].state, "timeout", "a silent nc that used its whole wait");
    }

    #[test]
    fn dns_results() {
        let c = Check::Dns { name: "example.com".into(), record: "A".into(), resolver: String::new() };
        let r = c.parse(0, "#tool=dig\n;; Got answer:\n;; ->>HEADER<<- opcode: QUERY, status: NOERROR, id: 1\n\n;; ANSWER SECTION:\nexample.com.\t300\tIN\tA\t93.184.215.14\nexample.com.\t300\tIN\tA\t93.184.215.15\n\n;; Query time: 12 msec\n;; SERVER: 10.0.0.2#53(10.0.0.2) (UDP)\n");
        assert_eq!(r.records.iter().map(|x| (x.name.as_str(), x.ttl, x.r#type.as_str(), x.value.as_str())).collect::<Vec<_>>(), [("example.com", Some(300), "A", "93.184.215.14"), ("example.com", Some(300), "A", "93.184.215.15")]);
        assert_eq!((r.ok, r.verdict.as_str()), (true, "example.com → 93.184.215.14 and 1 more"));
        assert_eq!(r.facts.iter().map(|f| (f.label.as_str(), f.value.as_str())).collect::<Vec<_>>(), [("Answered in", "12 ms"), ("Resolver", "10.0.0.2#53")]);
        let r = c.parse(0, "#tool=dig\n;; ->>HEADER<<- opcode: QUERY, status: NXDOMAIN, id: 1\n");
        assert_eq!((r.ok, r.verdict.as_str()), (false, "example.com does not exist (NXDOMAIN)"));
        let r = c.parse(0, "#tool=dig\n;; ->>HEADER<<- opcode: QUERY, status: NOERROR, id: 1\n");
        assert_eq!(r.verdict, "example.com exists, but has no A record");
        let r = c.parse(0, "#tool=host\nexample.com has address 93.184.215.14\nexample.com mail is handled by 10 mx.example.com.\n");
        assert_eq!(r.records.iter().map(|x| (x.r#type.as_str(), x.value.as_str())).collect::<Vec<_>>(), [("A", "93.184.215.14"), ("MX", "10 mx.example.com")]);
        let r = c.parse(1, "#tool=host\nHost example.com not found: 3(NXDOMAIN)\n");
        assert_eq!(r.verdict, "example.com does not exist (NXDOMAIN)");
        let r = c.parse(0, "#tool=nslookup\nServer:\t\t10.0.0.2\nAddress:\t10.0.0.2#53\n\nName:\texample.com\nAddress: 93.184.215.14\n");
        assert_eq!(r.records.iter().map(|x| x.value.as_str()).collect::<Vec<_>>(), ["93.184.215.14"]);
        let r = c.parse(0, "#tool=getent\n93.184.215.14  STREAM example.com\n93.184.215.14  DGRAM\n");
        assert_eq!(r.records.len(), 1);
    }

    #[test]
    fn trace_results() {
        let c = Check::Trace("db".into());
        let r = c.parse(0, "#tool=traceroute\ntraceroute to db (10.0.2.5), 20 hops max, 60 byte packets\n 1  10.0.0.1  0.412 ms\n 2  *\n 3  10.0.2.5  1.200 ms\n");
        assert_eq!(r.hops.iter().map(|h| (h.n, h.addr.as_str(), h.ms)).collect::<Vec<_>>(), [(1, "10.0.0.1", Some(0.412)), (2, "", None), (3, "10.0.2.5", Some(1.2))]);
        assert_eq!((r.level.as_str(), r.verdict.as_str()), ("ok", "db reached in 3 hops, 1.2 ms"));
        let r = c.parse(0, "#tool=tracepath\n 1?: [LOCALHOST]                      pmtu 1500\n 1:  10.0.0.1                                          0.412ms \n 1:  10.0.0.1                                          0.380ms \n 2:  no reply\n");
        assert_eq!(r.hops.iter().map(|h| (h.n, h.addr.as_str())).collect::<Vec<_>>(), [(1, "10.0.0.1"), (2, "")]);
        assert_eq!(r.level, "warn");
        assert!(r.verdict.contains("the last answer came from 10.0.0.1 (hop 1)"), "{}", r.verdict);
    }

    #[test]
    fn http_results() {
        let c = Check::Http("https://x/".into());
        let r = c.parse(0, "#tool=curl\ncode=200\nip=10.0.0.5\ndns=0.010\nconnect=0.030\ntls=0.090\nfirst=0.150\ntotal=0.200\nredirects=1\nurl=https://x/home?token=abc\nsize=512\n");
        assert_eq!((r.ok, r.verdict.as_str()), (true, "HTTP 200 in 200 ms"));
        assert_eq!(r.facts.iter().map(|f| (f.label.as_str(), f.value.as_str())).collect::<Vec<_>>(), [("Status", "200"), ("Answered by", "10.0.0.5"), ("DNS", "10 ms"), ("Connect", "20 ms"), ("TLS", "60 ms"), ("First byte", "60 ms"), ("Total", "200 ms"), ("Redirects", "1 → https://x/home")]);
        assert!(!r.output.contains("token"), "the final URL's query string is not echoed");
        let r = c.parse(0, "#tool=curl\ncode=503\nip=10.0.0.5\ndns=0\nconnect=0.001\ntls=0\nfirst=0.002\ntotal=0.002\nredirects=0\nurl=http://x/\n");
        assert_eq!((r.ok, r.verdict.as_str()), (false, "HTTP 503 in 2.0 ms"));
        let r = c.parse(7, "#tool=curl\ncurl: (7) Failed to connect to x port 443: Connection refused\ncode=000\ntotal=0.001\n");
        assert_eq!((r.ok, r.verdict.as_str()), (false, "Failed to connect to x port 443: Connection refused"));
        let r = c.parse(0, "#tool=wget\nSpider mode enabled.\n  HTTP/1.1 301 Moved\n  HTTP/1.1 200 OK\n");
        assert_eq!((r.ok, r.verdict.as_str()), (true, "HTTP 200"));
    }

    #[test]
    fn tls_results() {
        let c = Check::Tls("example.com".into(), 443);
        let day = |d: i64| (Utc::now() + chrono::Duration::days(d)).format("%b %e %H:%M:%S %Y GMT").to_string();
        let out = |until: &str, verify: &str| format!("#tool=openssl\nNew, TLSv1.3, Cipher is TLS_AES_256_GCM_SHA384\n{verify}\nsubject=CN = example.com\nissuer=C = US, O = Let's Encrypt, CN = R11\nnotBefore=Jan  5 00:00:00 2026 GMT\nnotAfter={until}\nX509v3 Subject Alternative Name: \n    DNS:example.com, DNS:www.example.com\n");
        let r = c.parse(0, &out(&day(90), "    Verify return code: 0 (ok)"));
        assert_eq!((r.ok, r.level.as_str()), (true, "ok"), "{}", r.verdict);
        assert!(r.verdict.starts_with("valid for 89 more days, issued by R11") || r.verdict.starts_with("valid for 90 more days, issued by R11"), "{}", r.verdict);
        let f = |l: &str| r.facts.iter().find(|x| x.label == l).map(|x| x.value.clone()).unwrap_or_default();
        assert_eq!((f("Issued to"), f("Issued by"), f("Names"), f("Valid from"), f("Protocol"), f("Trusted by this server")), ("example.com".into(), "R11".into(), "example.com, www.example.com".into(), "2026-01-05".into(), "TLSv1.3 · TLS_AES_256_GCM_SHA384".into(), "yes".into()));
        let r = c.parse(0, &out(&day(5), "Verification: OK"));
        assert_eq!(r.level, "warn", "{}", r.verdict);
        let r = c.parse(0, &out(&day(-3), "Verification error: certificate has expired"));
        assert_eq!((r.ok, r.verdict.as_str()), (false, "the certificate of example.com:443 expired 3 days ago"));
        let r = c.parse(0, &out(&day(90), "    Verify return code: 18 (self-signed certificate)"));
        assert_eq!((r.ok, r.verdict.as_str()), (false, "example.com:443 is not trusted from this server: self-signed certificate"));
        let r = c.parse(0, "#tool=openssl\nconnect:errno=111\nunable to load certificate\n");
        assert_eq!((r.ok, r.verdict.as_str()), (false, "no certificate from example.com:443 — the connection was refused"));
    }

    #[test]
    fn listening_ports() {
        let r = Check::Listen.parse(0, "#tool=ss\nNetid State  Recv-Q Send-Q Local Address:Port Peer Address:Port Process\nudp   UNCONN 0      0      127.0.0.53%lo:53   0.0.0.0:*\ntcp   LISTEN 0      511    0.0.0.0:80         0.0.0.0:*    users:((\"nginx\",pid=812,fd=6),(\"nginx\",pid=811,fd=6))\ntcp   LISTEN 0      128    [::]:22            [::]:*       users:((\"sshd\",pid=700,fd=4))\ntcp   LISTEN 0      128    127.0.0.1:5432     0.0.0.0:*\n");
        assert_eq!(
            r.listeners.iter().map(|l| (l.proto.as_str(), l.addr.as_str(), l.port, l.process.as_str(), l.pid)).collect::<Vec<_>>(),
            [("tcp", "::", 22, "sshd", Some(700)), ("udp", "127.0.0.53%lo", 53, "", None), ("tcp", "0.0.0.0", 80, "nginx", Some(812)), ("tcp", "127.0.0.1", 5432, "", None)]
        );
        assert_eq!((r.ok, r.verdict.as_str()), (true, "4 listening ports, 2 reachable from other machines"));
        let r = Check::Listen.parse(0, "#tool=netstat\nActive Internet connections (only servers)\nProto Recv-Q Send-Q Local Address  Foreign Address  State  PID/Program name\ntcp        0      0 0.0.0.0:22     0.0.0.0:*        LISTEN 700/sshd\ntcp6       0      0 :::80          :::*             LISTEN -\n");
        assert_eq!(r.listeners.iter().map(|l| (l.addr.as_str(), l.port, l.process.as_str(), l.pid)).collect::<Vec<_>>(), [("0.0.0.0", 22, "sshd", Some(700)), ("::", 80, "", None)]);
    }
}
