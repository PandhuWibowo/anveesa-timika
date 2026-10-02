//! Reading a server's metrics over SSH: one POSIX `sh` script (Linux `/proc`
//! and `/sys`, plus `df`, `docker` and `systemctl` when present), its output
//! parsed into `Raw`. Counters (CPU jiffies, network and disk bytes) become
//! rates by comparing two readings.

use serde::{Deserialize, Serialize};

/// Sent on stdin to `sh -s`. Read-only; every section is optional. The first
/// line identifies it (the test SSH target answers it from a file).
pub const SCRIPT: &str = r#"# timika-metrics v1
export LC_ALL=C PATH="$PATH:/usr/sbin:/sbin:/usr/local/bin"
if [ ! -r /proc/stat ]; then echo "unsupported=$(uname -s 2>/dev/null)"; exit 0; fi
echo "now=$(date +%s)"
echo "os=$(. /etc/os-release 2>/dev/null; echo "${PRETTY_NAME:-Linux}")"
echo "kernel=$(uname -r)"
echo "cpus=$(grep -c '^processor' /proc/cpuinfo 2>/dev/null)"
echo "model=$(sed -n 's/^model name[[:space:]]*:[[:space:]]*//p' /proc/cpuinfo 2>/dev/null | head -n 1)"
echo "uptime=$(cut -d. -f1 /proc/uptime)"
echo "stat=$(head -n 1 /proc/stat)"
echo "load=$(cut -d' ' -f1-3 /proc/loadavg)"
awk '/^(MemTotal|MemAvailable|MemFree|Buffers|Cached|SwapTotal|SwapFree):/ { sub(":", "", $1); print "mem." $1 "=" $2 }' /proc/meminfo
awk 'NR > 2 { gsub(":", " "); if ($1 != "lo") { rx += $2; tx += $10 } } END { printf "net=%.0f %.0f\n", rx, tx }' /proc/net/dev
r=0; w=0
for d in /sys/block/*; do
  n=${d##*/}
  case "$n" in loop*|ram*|zram*|sr*|fd*|dm-*|md*) continue ;; esac
  [ -r "$d/stat" ] || continue
  set -- $(cat "$d/stat")
  r=$((r + $3)); w=$((w + $7))
done
echo "io=$r $w"
t=
for f in /sys/class/thermal/thermal_zone*/temp /sys/class/hwmon/hwmon*/temp*_input; do
  [ -r "$f" ] || continue
  v=$(cat "$f" 2>/dev/null) || continue
  case "$v" in ''|*[!0-9]*) continue ;; esac
  if [ -z "$t" ] || [ "$v" -gt "$t" ]; then t=$v; fi
done
[ -n "$t" ] && echo "temp=$t"
df -kP 2>/dev/null | awk 'NR > 1 { print "disk=" $1 "|" $6 "|" $2 "|" $3 }'
# A stuck docker daemon must not make the whole reading time out.
T=; command -v timeout >/dev/null 2>&1 && T="timeout 8"
if command -v docker >/dev/null 2>&1 && $T docker ps -a --format 'ctr={{.Names}}|{{.State}}|{{.Image}}|{{.Status}}|{{.Ports}}' 2>/dev/null; then
  ($T docker stats --no-stream --format 'cst={{.Name}}|{{.CPUPerc}}|{{.MemUsage}}|{{.NetIO}}' 2>/dev/null) || true
fi
if command -v systemctl >/dev/null 2>&1; then
  $T systemctl --failed --no-legend --plain 2>/dev/null | awk '$1 != "" { print "unit=" $1 }'
fi
echo "end=1"
"#;

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct Disk {
    pub device: String,
    pub mount: String,
    pub total: u64,
    pub used: u64,
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct Container {
    pub name: String,
    /// running · exited · paused · restarting · created · dead
    pub state: String,
    #[serde(default)]
    pub image: String,
    /// "Up 3 hours (healthy)", "Exited (0) 2 days ago"
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub ports: String,
    /// healthy · unhealthy · starting (from the status), or empty.
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub cpu: Option<f64>,
    #[serde(default)]
    pub mem: u64,
    #[serde(default)]
    pub mem_limit: u64,
    /// Bytes received / sent since the container started (for rates).
    #[serde(default)]
    pub net: Option<(u64, u64)>,
    /// Bytes per second, from two readings.
    #[serde(default)]
    pub rx: Option<f64>,
    #[serde(default)]
    pub tx: Option<f64>,
}

/// One reading, counters still absolute.
#[derive(Clone, Default, Debug)]
pub struct Raw {
    pub now: i64,
    pub os: String,
    pub kernel: String,
    pub cpu_model: String,
    pub cpus: u32,
    pub uptime: u64,
    /// (busy, total) jiffies since boot.
    pub cpu: Option<(u64, u64)>,
    pub load: Option<(f64, f64, f64)>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// (rx, tx) bytes since boot.
    pub net: Option<(u64, u64)>,
    /// (read, written) bytes since boot.
    pub io: Option<(u64, u64)>,
    pub temp: Option<f64>,
    pub disks: Vec<Disk>,
    pub containers: Vec<Container>,
    pub failed_units: Vec<String>,
}

/// Docker's own rule for names, so a name is always safe as one shell word.
pub fn valid_container(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) && !name.starts_with('-')
}

/// File systems that aren't real storage.
fn pseudo(device: &str, mount: &str) -> bool {
    matches!(device, "tmpfs" | "devtmpfs" | "overlay" | "none" | "udev" | "shm" | "efivarfs" | "squashfs" | "rootfs" | "run" | "dev" | "cgroup" | "cgroup2" | "proc" | "sysfs")
        || device.starts_with("/dev/loop")
        || mount.starts_with("/snap/")
        || mount.starts_with("/var/lib/docker/")
        || mount.starts_with("/run/")
        || mount.starts_with("/sys/")
        || mount.starts_with("/proc/")
        || mount.starts_with("/dev/")
        || mount == "/dev"
        || mount.starts_with("/boot/efi")
}

/// "12.5MiB / 1.9GiB" → bytes used.
fn mem_bytes(s: &str) -> u64 {
    size_bytes(s.split('/').next().unwrap_or(""))
}

/// "12.5MiB", "1.2kB", "0B" → bytes.
fn size_bytes(s: &str) -> u64 {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let n: f64 = s[..split].trim().parse().unwrap_or(0.0);
    let mult = match s[split..].trim() {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0f64.powi(4),
        _ => 1.0,
    };
    (n * mult) as u64
}

pub fn parse(out: &str) -> Result<Raw, String> {
    let mut r = Raw::default();
    let (mut mem_free, mut buffers, mut cached, mut mem_avail, mut swap_free) = (0u64, 0u64, 0u64, None, 0u64);
    let mut complete = false;
    for line in out.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let nums = || v.split_whitespace().filter_map(|x| x.parse::<f64>().ok()).collect::<Vec<f64>>();
        match k {
            "unsupported" => return Err(format!("only Linux servers can be monitored (this one is {})", if v.is_empty() { "unknown" } else { v })),
            "now" => r.now = v.parse().unwrap_or(0),
            "os" => r.os = v.chars().take(80).collect(),
            "kernel" => r.kernel = v.chars().take(80).collect(),
            "model" => r.cpu_model = v.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect(),
            "cpus" => r.cpus = v.parse().unwrap_or(0),
            "uptime" => r.uptime = v.parse().unwrap_or(0),
            "stat" => {
                // cpu user nice system idle iowait irq softirq steal …
                let n: Vec<u64> = v.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
                if n.len() >= 4 {
                    let total: u64 = n.iter().take(8).sum();
                    let idle = n[3] + n.get(4).copied().unwrap_or(0);
                    r.cpu = Some((total.saturating_sub(idle), total));
                }
            }
            "load" => {
                let n = nums();
                if n.len() == 3 {
                    r.load = Some((n[0], n[1], n[2]));
                }
            }
            "mem.MemTotal" => r.mem_total = v.parse::<u64>().unwrap_or(0) * 1024,
            "mem.MemAvailable" => mem_avail = v.parse::<u64>().ok().map(|x| x * 1024),
            "mem.MemFree" => mem_free = v.parse::<u64>().unwrap_or(0) * 1024,
            "mem.Buffers" => buffers = v.parse::<u64>().unwrap_or(0) * 1024,
            "mem.Cached" => cached = v.parse::<u64>().unwrap_or(0) * 1024,
            "mem.SwapTotal" => r.swap_total = v.parse::<u64>().unwrap_or(0) * 1024,
            "mem.SwapFree" => swap_free = v.parse::<u64>().unwrap_or(0) * 1024,
            "net" => {
                let n = nums();
                if n.len() == 2 {
                    r.net = Some((n[0] as u64, n[1] as u64));
                }
            }
            "io" => {
                let n = nums();
                if n.len() == 2 {
                    // 512-byte sectors.
                    r.io = Some((n[0] as u64 * 512, n[1] as u64 * 512));
                }
            }
            "temp" => r.temp = v.parse::<f64>().ok().map(|t| if t > 1000.0 { t / 1000.0 } else { t }).filter(|t| (1.0..150.0).contains(t)),
            "disk" => {
                let f: Vec<&str> = v.split('|').collect();
                if let [device, mount, total, used] = f[..] {
                    let (total, used) = (total.parse::<u64>().unwrap_or(0) * 1024, used.parse::<u64>().unwrap_or(0) * 1024);
                    if total > 0 && !pseudo(device, mount) && !r.disks.iter().any(|d: &Disk| d.device == device) && r.disks.len() < 30 {
                        r.disks.push(Disk { device: device.into(), mount: mount.into(), total, used });
                    }
                }
            }
            "ctr" => {
                let f: Vec<&str> = v.splitn(5, '|').collect();
                if f.len() >= 2 && valid_container(f[0]) && r.containers.len() < 200 {
                    let status = f.get(3).copied().unwrap_or("").to_string();
                    let health = ["unhealthy", "healthy", "health: starting"].iter().find(|h| status.contains(&format!("({h})"))).map(|h| h.trim_start_matches("health: ").to_string()).unwrap_or_default();
                    r.containers.push(Container {
                        name: f[0].into(),
                        state: f[1].into(),
                        image: f.get(2).copied().unwrap_or("").chars().take(200).collect(),
                        status: status.chars().take(120).collect(),
                        ports: f.get(4).copied().unwrap_or("").chars().take(300).collect(),
                        health,
                        ..Default::default()
                    });
                }
            }
            "cst" => {
                let f: Vec<&str> = v.split('|').collect();
                if f.len() >= 3 {
                    if let Some(c) = r.containers.iter_mut().find(|c| c.name == f[0]) {
                        c.cpu = f[1].trim_end_matches('%').parse().ok();
                        c.mem = mem_bytes(f[2]);
                        c.mem_limit = f[2].split('/').nth(1).map(size_bytes).unwrap_or(0);
                        if let Some((rx, tx)) = f.get(3).and_then(|n| n.split_once('/')) {
                            c.net = Some((size_bytes(rx), size_bytes(tx)));
                        }
                    }
                }
            }
            "unit" => {
                if r.failed_units.len() < 50 {
                    r.failed_units.push(v.chars().take(100).collect());
                }
            }
            "end" => complete = true,
            _ => {}
        }
    }
    if !complete || r.mem_total == 0 {
        return Err("the server didn't return its metrics (is /proc readable and `sh` available?)".into());
    }
    let avail = mem_avail.unwrap_or(mem_free + buffers + cached);
    r.mem_used = r.mem_total.saturating_sub(avail);
    r.swap_used = r.swap_total.saturating_sub(swap_free);
    r.containers.sort_by(|a, b| (a.state != "running").cmp(&(b.state != "running")).then(a.name.cmp(&b.name)));
    Ok(r)
}

/// Rates between two readings (None when there is no earlier one, the clock
/// didn't advance, or a counter went backwards — a reboot).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rates {
    pub cpu: Option<f64>,
    pub rx: Option<f64>,
    pub tx: Option<f64>,
    pub io_read: Option<f64>,
    pub io_write: Option<f64>,
}

pub fn rates(prev: Option<&Raw>, cur: &Raw) -> Rates {
    let Some(p) = prev else { return Rates::default() };
    let dt = (cur.now - p.now) as f64;
    if dt <= 0.0 || cur.uptime < p.uptime {
        return Rates::default();
    }
    let per_sec = |a: u64, b: u64| (b >= a).then(|| (b - a) as f64 / dt);
    let cpu = match (p.cpu, cur.cpu) {
        (Some((b0, t0)), Some((b1, t1))) if t1 > t0 && b1 >= b0 => Some(((b1 - b0) as f64 / (t1 - t0) as f64 * 100.0).clamp(0.0, 100.0)),
        _ => None,
    };
    let pair = |a: Option<(u64, u64)>, b: Option<(u64, u64)>| match (a, b) {
        (Some(a), Some(b)) => (per_sec(a.0, b.0), per_sec(a.1, b.1)),
        _ => (None, None),
    };
    let (rx, tx) = pair(p.net, cur.net);
    let (io_read, io_write) = pair(p.io, cur.io);
    Rates { cpu, rx, tx, io_read, io_write }
}

/// Fill each container's network rates from the previous reading (a counter
/// that went down means the container restarted: no rate this time).
pub fn container_rates(prev: Option<&Raw>, cur: &mut Raw) {
    let Some(p) = prev else { return };
    let dt = (cur.now - p.now) as f64;
    if dt <= 0.0 {
        return;
    }
    for c in &mut cur.containers {
        let before = p.containers.iter().find(|x| x.name == c.name).and_then(|x| x.net);
        if let (Some((r0, t0)), Some((r1, t1))) = (before, c.net) {
            if r1 >= r0 && t1 >= t0 {
                c.rx = Some((r1 - r0) as f64 / dt);
                c.tx = Some((t1 - t0) as f64 / dt);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "now=1790000060\nos=Ubuntu 24.04.1 LTS\nkernel=6.8.0-45-generic\ncpus=2\nmodel=Intel(R) Xeon(R)   Platinum 8255C CPU @ 2.50GHz\nuptime=86460\n\
stat=cpu  2000 100 900 16000 400 0 50 50 0 0\nload=0.42 0.30 0.25\n\
mem.MemTotal=2000000\nmem.MemFree=200000\nmem.MemAvailable=1500000\nmem.Buffers=50000\nmem.Cached=900000\nmem.SwapTotal=1000000\nmem.SwapFree=750000\n\
net=6000000 3000000\nio=20000 40000\ntemp=54000\n\
disk=/dev/vda1|/|41152736|20576368\ndisk=tmpfs|/run|200000|1000\ndisk=/dev/vdb|/data|103081248|10308124\ndisk=overlay|/var/lib/docker/overlay2/x/merged|41152736|20576368\ndisk=/dev/loop3|/snap/core/1|100|100\n\
ctr=web|running|nginx:1.27|Up 3 hours (healthy)|0.0.0.0:8080->80/tcp, [::]:8080->80/tcp\nctr=old-job|exited\nctr=bad name; rm|running\ncst=web|12.50%|128MiB / 1.9GiB|1.5MB / 300kB\nunit=nginx.service\nend=1\n";

    #[test]
    fn parses_a_reading() {
        let r = parse(OUT).unwrap();
        assert_eq!((r.os.as_str(), r.cpus, r.uptime), ("Ubuntu 24.04.1 LTS", 2, 86460));
        assert_eq!(r.cpu_model, "Intel(R) Xeon(R) Platinum 8255C CPU @ 2.50GHz");
        // busy = total(19500) - idle(16000) - iowait(400)
        assert_eq!(r.cpu, Some((3100, 19500)));
        assert_eq!((r.mem_total, r.mem_used), (2000000 * 1024, 500000 * 1024));
        assert_eq!((r.swap_total, r.swap_used), (1000000 * 1024, 250000 * 1024));
        assert_eq!(r.load, Some((0.42, 0.30, 0.25)));
        assert_eq!(r.temp, Some(54.0));
        assert_eq!(r.io, Some((20000 * 512, 40000 * 512)));
        assert_eq!(r.disks.iter().map(|d| d.mount.as_str()).collect::<Vec<_>>(), ["/", "/data"], "pseudo file systems skipped");
        assert_eq!(r.containers.len(), 2);
        assert_eq!((r.containers[0].name.as_str(), r.containers[0].cpu, r.containers[0].mem), ("web", Some(12.5), 128 * 1024 * 1024));
        let w = &r.containers[0];
        assert_eq!((w.image.as_str(), w.status.as_str(), w.health.as_str()), ("nginx:1.27", "Up 3 hours (healthy)", "healthy"));
        assert_eq!((w.ports.as_str(), w.net, w.mem_limit), ("0.0.0.0:8080->80/tcp, [::]:8080->80/tcp", Some((1_500_000, 300_000)), (1.9 * 1024.0 * 1024.0 * 1024.0) as u64));
        assert_eq!((r.containers[1].name.as_str(), r.containers[1].image.as_str()), ("old-job", ""), "the short form still parses; odd names are dropped");
        assert_eq!(r.failed_units, ["nginx.service"]);
    }

    #[test]
    fn rates_between_readings() {
        let a = parse(&OUT.replace("now=1790000060", "now=1790000000").replace("uptime=86460", "uptime=86400").replace("stat=cpu  2000 100 900 16000 400", "stat=cpu  1000 100 500 15000 400").replace("net=6000000 3000000", "net=0 0").replace("io=20000 40000", "io=8000 40000")).unwrap();
        let b = parse(OUT).unwrap();
        let r = rates(Some(&a), &b);
        // busy +1400 of total +2400 jiffies
        assert_eq!(r.cpu.map(|c| c.round()), Some(58.0));
        assert_eq!((r.rx, r.tx), (Some(100000.0), Some(50000.0)));
        assert_eq!((r.io_read, r.io_write), (Some(12000.0 * 512.0 / 60.0), Some(0.0)));
        assert_eq!(rates(None, &b), Rates::default());
        let mut older = a.clone();
        older.containers[0].net = Some((300_000, 0));
        let mut cur = b.clone();
        container_rates(Some(&older), &mut cur);
        assert_eq!((cur.containers[0].rx, cur.containers[0].tx), (Some(20_000.0), Some(5_000.0)), "1.2 MB in, 300 kB out over 60 s");
        older.containers[0].net = Some((9_000_000, 0));
        let mut cur = b.clone();
        container_rates(Some(&older), &mut cur);
        assert_eq!(cur.containers[0].rx, None, "counter went down: the container restarted");
        assert_eq!(rates(Some(&b), &a), Rates::default(), "clock went back / reboot");
    }

    #[test]
    fn refuses_bad_output() {
        assert!(parse("unsupported=Darwin\n").unwrap_err().contains("only Linux"));
        assert!(parse("now=1\nmem.MemTotal=5\n").is_err(), "cut off (no end marker)");
        assert!(parse("").is_err());
        // Old kernels without MemAvailable.
        let r = parse("mem.MemTotal=1000\nmem.MemFree=100\nmem.Buffers=50\nmem.Cached=250\nend=1\n").unwrap();
        assert_eq!(r.mem_used, 600 * 1024);
    }

    #[test]
    fn docker_memory_units() {
        assert_eq!(mem_bytes("1.5GiB / 8GiB"), (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(mem_bytes("512kB / 1GB"), 512_000);
        assert_eq!(mem_bytes("0B / 0B"), 0);
    }
}
